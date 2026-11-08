//! Moving an owner file together with its sidecars: the names the sidecars take, the conflicts
//! they meet, the links they keep, what a failing member does to the group, and a group that is
//! killed half way (recovery then decides member by member: each member is its own journal row).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use Step::{
    AfterCopy, AfterFinalRename, AfterFsync, AfterJournal, AfterPartCreate, AfterVerify,
    BeforeIndexTx, BeforeUnlink,
};
use apm_core::dto::common::ConflictPolicy;
use apm_core::hash::sha1_bytes;
use apm_core::sidecar::LinkKind;
use apm_server::fsops::{
    AppliedOp, Expect, FailAction, Failpoints, FsOpError, GroupOutcome, Loc, MoveGroup,
    MoveOutcome, MoveRequest, OpContext, OpHook, SidecarAction, SidecarItem, SidecarOutcome,
    SidecarSkip, SkipReason, Step,
};
use apm_server::indexer::RecordingChanges;
use common::BASE_MTIME;
use common::fsops::{
    ALL_STEPS, Fx, SplitMover, Trap, crash_fx, disk_dirs, disk_files, loc, recover, request,
};
use futures_util::future::BoxFuture;
use sqlx::SqliteConnection;
use tokio_util::sync::CancellationToken;

const PHOTO: &[u8] = b"the bytes of one photograph";
const OTHER: &[u8] = b"the bytes of another one";
const RAW: &[u8] = b"a raw file that shares the stem";
const EXACT: &[u8] = b"<x:xmpmeta>exact</x:xmpmeta>";
const STEM: &[u8] = b"<x:xmpmeta>stem</x:xmpmeta>";
const STRANGER: &[u8] = b"<x:xmpmeta>somebody else's</x:xmpmeta>";

fn cx() -> OpContext {
    OpContext::detached()
}

fn len(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap()
}

/// The index rows of `x/IMG_1.jpg` and of the two sidecars next to it.
#[derive(Debug, Clone, Copy)]
struct Trio {
    owner: i64,
    exact: i64,
    stem: i64,
}

/// `x/IMG_1.jpg`, its exact sidecar `IMG_1.jpg.xmp` and its stem sidecar `IMG_1.xmp`, scanned.
async fn trio(fx: &Fx) -> Trio {
    fx.ta.write("x/IMG_1.jpg", PHOTO);
    fx.ta.write("x/IMG_1.jpg.xmp", EXACT);
    fx.ta.write("x/IMG_1.xmp", STEM);
    fx.scan().await;
    Trio {
        owner: fx.row_id(fx.a, "x/IMG_1.jpg").await,
        exact: fx.row_id(fx.a, "x/IMG_1.jpg.xmp").await,
        stem: fx.row_id(fx.a, "x/IMG_1.xmp").await,
    }
}

fn sidecar(fx: &Fx, name: &str, kind: LinkKind, action: SidecarAction) -> SidecarItem {
    SidecarItem {
        file_id: None,
        src: loc(fx.a, &format!("x/{name}")),
        kind,
        action,
    }
}

/// The group that moves the trio's owner to `dst`, both sidecars with it.
fn trio_group(fx: &Fx, dst: Loc, policy: ConflictPolicy) -> MoveGroup {
    MoveGroup {
        owner: request(loc(fx.a, "x/IMG_1.jpg"), dst, policy),
        sidecars: vec![
            sidecar(fx, "IMG_1.jpg.xmp", LinkKind::Exact, SidecarAction::Move),
            sidecar(fx, "IMG_1.xmp", LinkKind::Stem, SidecarAction::Move),
        ],
    }
}

async fn run(fx: &Fx, group: MoveGroup) -> GroupOutcome {
    fx.ops.move_group(group, &cx()).await.unwrap()
}

type Links = BTreeSet<(String, String, String)>;

fn links_of(links: &[(&str, &str, &str)]) -> Links {
    links
        .iter()
        .map(|(owner, sidecar, kind)| {
            (
                (*owner).to_owned(),
                (*sidecar).to_owned(),
                (*kind).to_owned(),
            )
        })
        .collect()
}

fn owner_moved(dst: Loc, renamed: bool, content: &[u8], file_id: i64) -> MoveOutcome {
    MoveOutcome::Moved {
        dst,
        renamed,
        cross_device: false,
        bytes: len(content),
        file_id,
    }
}

fn moved(dst: Loc, file_id: i64) -> SidecarOutcome {
    SidecarOutcome::Moved { dst, file_id }
}

/// What a copy across volumes leaves in the `hash` column: the digest it verified for a photo,
/// nothing for a sidecar (the indexer never hashes sidecars).
fn recorded_hash(path: &str, content: &[u8]) -> Option<Vec<u8>> {
    (!path.ends_with(".xmp")).then(|| sha1_bytes(content).to_vec())
}

/// Everything that says where files are: the disk, the index, the links and the journal.
#[derive(Debug, PartialEq, Eq)]
struct World {
    files: BTreeMap<String, Vec<u8>>,
    dirs: BTreeSet<String>,
    paths: BTreeSet<String>,
    links: Links,
    journal: Vec<(String, String)>,
}

async fn world(fx: &Fx) -> World {
    World {
        files: disk_files(fx.ta.root()),
        dirs: disk_dirs(fx.ta.root()),
        paths: fx.paths(fx.a).await,
        links: fx.links().await,
        journal: fx.journal().await,
    }
}

// --- names and links ---------------------------------------------------------------------------

#[tokio::test]
async fn a_group_moves_an_owner_with_both_kinds_of_sidecar_and_keeps_them_linked() {
    let fx = Fx::new().await;
    let ids = trio(&fx).await;
    fx.ta.write("x/IMG_2.jpg", OTHER);
    fx.ta.write("x/IMG_2.xmp", STRANGER);
    fx.scan().await;
    assert_eq!(
        fx.links().await,
        links_of(&[
            ("a:x/IMG_1.jpg", "a:x/IMG_1.jpg.xmp", "exact"),
            ("a:x/IMG_1.jpg", "a:x/IMG_1.xmp", "stem"),
            ("a:x/IMG_2.jpg", "a:x/IMG_2.xmp", "stem"),
        ])
    );
    let census = fx.content_census();
    let changes = Arc::new(RecordingChanges::new());
    let context = cx().with_changes(changes.clone());
    let group = trio_group(&fx, loc(fx.a, "y/IMG_1.jpg"), ConflictPolicy::Smart);

    let outcome = fx.ops.move_group(group, &context).await.unwrap();

    let expected = GroupOutcome {
        owner: owner_moved(loc(fx.a, "y/IMG_1.jpg"), false, PHOTO, ids.owner),
        sidecars: vec![
            moved(loc(fx.a, "y/IMG_1.jpg.xmp"), ids.exact),
            moved(loc(fx.a, "y/IMG_1.xmp"), ids.stem),
        ],
    };
    assert_eq!(outcome, expected);
    for (path, content, id) in [
        ("y/IMG_1.jpg", PHOTO, ids.owner),
        ("y/IMG_1.jpg.xmp", EXACT, ids.exact),
        ("y/IMG_1.xmp", STEM, ids.stem),
    ] {
        assert_eq!(fx.present(fx.a, path, content, "moved").await, id, "{path}");
    }
    for path in ["x/IMG_1.jpg", "x/IMG_1.jpg.xmp", "x/IMG_1.xmp"] {
        fx.gone(fx.a, path, "moved").await;
    }
    fx.present(fx.a, "x/IMG_2.jpg", OTHER, "bystander").await;
    fx.present(fx.a, "x/IMG_2.xmp", STRANGER, "bystander").await;
    assert_eq!(
        fx.links().await,
        links_of(&[
            ("a:y/IMG_1.jpg", "a:y/IMG_1.jpg.xmp", "exact"),
            ("a:y/IMG_1.jpg", "a:y/IMG_1.xmp", "stem"),
            ("a:x/IMG_2.jpg", "a:x/IMG_2.xmp", "stem"),
        ])
    );
    let each_member = [("move", "done"); 3];
    fx.assert_journal(&each_member, "one row per member").await;
    assert_eq!(fx.content_census(), census);
    let dirs = changes.dirs(fx.a);
    assert!(dirs.contains("x") && dirs.contains("y"), "{dirs:?}");
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn an_owner_renamed_by_a_conflict_takes_its_sidecars_to_the_same_new_name() {
    for policy in [ConflictPolicy::Smart, ConflictPolicy::Rename] {
        let fx = Fx::new().await;
        let ids = trio(&fx).await;
        fx.ta.write("y/IMG_1.jpg", OTHER);
        fx.scan().await;

        let outcome = run(&fx, trio_group(&fx, loc(fx.a, "y/IMG_1.jpg"), policy)).await;

        let expected = GroupOutcome {
            owner: owner_moved(loc(fx.a, "y/IMG_1_1.jpg"), true, PHOTO, ids.owner),
            sidecars: vec![
                moved(loc(fx.a, "y/IMG_1_1.jpg.xmp"), ids.exact),
                moved(loc(fx.a, "y/IMG_1_1.xmp"), ids.stem),
            ],
        };
        assert_eq!(outcome, expected, "{policy:?}");
        fx.present(fx.a, "y/IMG_1.jpg", OTHER, "the file that was there")
            .await;
        let linked = links_of(&[
            ("a:y/IMG_1_1.jpg", "a:y/IMG_1_1.jpg.xmp", "exact"),
            ("a:y/IMG_1_1.jpg", "a:y/IMG_1_1.xmp", "stem"),
        ]);
        assert_eq!(fx.links().await, linked, "{policy:?}");
        fx.assert_in_sync().await;
    }
}

#[tokio::test]
async fn a_name_a_sidecar_would_need_is_not_free_for_the_owner() {
    let fx = Fx::new().await;
    let ids = trio(&fx).await;
    fx.ta.write("y/IMG_1.jpg", OTHER);
    fx.ta.write("y/IMG_1_1.xmp", STRANGER);
    fx.scan().await;

    let outcome = run(
        &fx,
        trio_group(&fx, loc(fx.a, "y/IMG_1.jpg"), ConflictPolicy::Smart),
    )
    .await;

    let expected = GroupOutcome {
        owner: owner_moved(loc(fx.a, "y/IMG_1_2.jpg"), true, PHOTO, ids.owner),
        sidecars: vec![
            moved(loc(fx.a, "y/IMG_1_2.jpg.xmp"), ids.exact),
            moved(loc(fx.a, "y/IMG_1_2.xmp"), ids.stem),
        ],
    };
    assert_eq!(outcome, expected);
    fx.present(fx.a, "y/IMG_1_1.xmp", STRANGER, "the stranger")
        .await;
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn the_owners_policy_rules_the_sidecars_but_a_sidecar_never_gets_a_new_name() {
    // (policy, what is at the sidecar's name, what happens to the sidecar)
    let cases = [
        (
            ConflictPolicy::Smart,
            OTHER,
            Some(SidecarSkip::SidecarConflict),
        ),
        (
            ConflictPolicy::Smart,
            EXACT,
            Some(SidecarSkip::AlreadyPresent),
        ),
        (
            ConflictPolicy::Rename,
            OTHER,
            Some(SidecarSkip::SidecarConflict),
        ),
        (
            ConflictPolicy::Rename,
            EXACT,
            Some(SidecarSkip::SidecarConflict),
        ),
        (
            ConflictPolicy::Skip,
            OTHER,
            Some(SidecarSkip::SidecarConflict),
        ),
        (
            ConflictPolicy::Skip,
            EXACT,
            Some(SidecarSkip::SidecarConflict),
        ),
        (ConflictPolicy::Replace, OTHER, None),
        (ConflictPolicy::Replace, EXACT, None),
    ];
    for (policy, there, skipped) in cases {
        let label = format!(
            "{policy:?} over {}",
            if there == EXACT { "the same" } else { "other" }
        );
        let fx = Fx::new().await;
        let ids = trio(&fx).await;
        fx.ta.write("y/IMG_1.jpg.xmp", there);
        fx.scan().await;

        let outcome = run(&fx, trio_group(&fx, loc(fx.a, "y/IMG_1.jpg"), policy)).await;

        assert_eq!(
            outcome.owner,
            owner_moved(loc(fx.a, "y/IMG_1.jpg"), false, PHOTO, ids.owner),
            "{label}"
        );
        let exact = match skipped {
            Some(skip) => SidecarOutcome::Skipped(skip),
            None => moved(loc(fx.a, "y/IMG_1.jpg.xmp"), ids.exact),
        };
        let expected = vec![exact, moved(loc(fx.a, "y/IMG_1.xmp"), ids.stem)];
        assert_eq!(outcome.sidecars, expected, "{label}");
        if skipped.is_some() {
            let kept = fx.present(fx.a, "x/IMG_1.jpg.xmp", EXACT, &label).await;
            assert_eq!(kept, ids.exact, "{label}: the sidecar stays where it was");
            fx.present(fx.a, "y/IMG_1.jpg.xmp", there, &label).await;
            assert!(fx.trash_items().await.is_empty(), "{label}");
        } else {
            let replaced = fx.present(fx.a, "y/IMG_1.jpg.xmp", EXACT, &label).await;
            assert_eq!(replaced, ids.exact, "{label}");
            fx.gone(fx.a, "x/IMG_1.jpg.xmp", &label).await;
            let trashed: Vec<String> = fx
                .trash_items()
                .await
                .into_iter()
                .map(|item| item.orig_path)
                .collect();
            assert_eq!(
                trashed,
                ["y/IMG_1.jpg.xmp"],
                "{label}: what was replaced is trashed"
            );
        }
        let linked = links_of(&[
            ("a:y/IMG_1.jpg", "a:y/IMG_1.jpg.xmp", "exact"),
            ("a:y/IMG_1.jpg", "a:y/IMG_1.xmp", "stem"),
        ]);
        assert_eq!(
            fx.links().await,
            linked,
            "{label}: the owner is linked to what is there"
        );
        fx.assert_in_sync_for(&label).await;
    }
}

#[tokio::test]
async fn replacing_the_owner_replaces_its_sidecars_and_trashes_every_old_file() {
    let fx = Fx::new().await;
    let ids = trio(&fx).await;
    fx.ta.write("y/IMG_1.jpg", OTHER);
    fx.ta.write("y/IMG_1.jpg.xmp", STRANGER);
    fx.ta.write("y/IMG_1.xmp", STRANGER);
    fx.scan().await;

    let outcome = run(
        &fx,
        trio_group(&fx, loc(fx.a, "y/IMG_1.jpg"), ConflictPolicy::Replace),
    )
    .await;

    let expected = GroupOutcome {
        owner: owner_moved(loc(fx.a, "y/IMG_1.jpg"), false, PHOTO, ids.owner),
        sidecars: vec![
            moved(loc(fx.a, "y/IMG_1.jpg.xmp"), ids.exact),
            moved(loc(fx.a, "y/IMG_1.xmp"), ids.stem),
        ],
    };
    assert_eq!(outcome, expected);
    let mut trashed: Vec<(String, Vec<u8>)> = Vec::new();
    for item in fx.trash_items().await {
        let content = fs::read(fx.ta.path(&item.trash_path)).unwrap();
        trashed.push((item.orig_path, content));
    }
    assert_eq!(
        trashed,
        [
            ("y/IMG_1.jpg".to_owned(), OTHER.to_vec()),
            ("y/IMG_1.jpg.xmp".to_owned(), STRANGER.to_vec()),
            ("y/IMG_1.xmp".to_owned(), STRANGER.to_vec()),
        ]
    );
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn a_skipped_owner_leaves_every_sidecar_where_it_is() {
    let cases = [
        (
            "the policy says skip",
            ConflictPolicy::Skip,
            OTHER,
            SkipReason::DestinationExists,
        ),
        (
            "the same photo is there",
            ConflictPolicy::Smart,
            PHOTO,
            SkipReason::IdenticalAtDestination,
        ),
    ];
    for (label, policy, there, reason) in cases {
        let fx = Fx::new().await;
        trio(&fx).await;
        fx.ta.write("y/IMG_1.jpg", there);
        fx.scan().await;
        let before = world(&fx).await;

        let outcome = run(&fx, trio_group(&fx, loc(fx.a, "y/IMG_1.jpg"), policy)).await;

        let expected = GroupOutcome {
            owner: MoveOutcome::Skipped(reason),
            sidecars: vec![SidecarOutcome::NotAttempted; 2],
        };
        assert_eq!(outcome, expected, "{label}");
        assert_eq!(world(&fx).await, before, "{label}: nothing changed");
        assert!(fx.journal().await.is_empty(), "{label}");
    }
}

#[tokio::test]
async fn a_group_that_is_already_in_place_does_nothing() {
    let fx = Fx::new().await;
    trio(&fx).await;
    let before = world(&fx).await;

    let outcome = run(
        &fx,
        trio_group(&fx, loc(fx.a, "x/IMG_1.jpg"), ConflictPolicy::Smart),
    )
    .await;

    let expected = GroupOutcome {
        owner: MoveOutcome::Skipped(SkipReason::SameLocation),
        sidecars: vec![SidecarOutcome::NotAttempted; 2],
    };
    assert_eq!(outcome, expected);
    assert_eq!(world(&fx).await, before);
}

#[tokio::test]
async fn renaming_the_owner_in_its_folder_renames_the_sidecars_too() {
    let fx = Fx::new().await;
    let ids = trio(&fx).await;

    let outcome = run(
        &fx,
        trio_group(&fx, loc(fx.a, "x/IMG_9.jpg"), ConflictPolicy::Smart),
    )
    .await;

    let expected = GroupOutcome {
        owner: owner_moved(loc(fx.a, "x/IMG_9.jpg"), false, PHOTO, ids.owner),
        sidecars: vec![
            moved(loc(fx.a, "x/IMG_9.jpg.xmp"), ids.exact),
            moved(loc(fx.a, "x/IMG_9.xmp"), ids.stem),
        ],
    };
    assert_eq!(outcome, expected);
    let files: BTreeSet<String> = disk_files(fx.ta.root()).into_keys().collect();
    let names = ["x/IMG_9.jpg", "x/IMG_9.jpg.xmp", "x/IMG_9.xmp"].map(str::to_owned);
    assert_eq!(files, BTreeSet::from(names));
    let linked = links_of(&[
        ("a:x/IMG_9.jpg", "a:x/IMG_9.jpg.xmp", "exact"),
        ("a:x/IMG_9.jpg", "a:x/IMG_9.xmp", "stem"),
    ]);
    assert_eq!(fx.links().await, linked);
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn changing_only_the_case_of_the_owner_changes_the_case_of_its_sidecars() {
    let fx = Fx::new().await;
    let ids = trio(&fx).await;

    let outcome = run(
        &fx,
        trio_group(&fx, loc(fx.a, "x/img_1.jpg"), ConflictPolicy::Smart),
    )
    .await;

    let expected = GroupOutcome {
        owner: owner_moved(loc(fx.a, "x/img_1.jpg"), false, PHOTO, ids.owner),
        sidecars: vec![
            moved(loc(fx.a, "x/img_1.jpg.xmp"), ids.exact),
            moved(loc(fx.a, "x/img_1.xmp"), ids.stem),
        ],
    };
    assert_eq!(outcome, expected);
    let files: BTreeSet<String> = disk_files(fx.ta.root()).into_keys().collect();
    let names = ["x/img_1.jpg", "x/img_1.jpg.xmp", "x/img_1.xmp"].map(str::to_owned);
    assert_eq!(files, BTreeSet::from(names), "the names on the disk");
    let linked = links_of(&[
        ("a:x/img_1.jpg", "a:x/img_1.jpg.xmp", "exact"),
        ("a:x/img_1.jpg", "a:x/img_1.xmp", "stem"),
    ]);
    assert_eq!(fx.links().await, linked);
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn a_stem_sidecar_shared_with_an_owner_that_stays_is_copied_not_moved() {
    let fx = Fx::new().await;
    fx.ta.write("x/IMG_1.jpg", PHOTO);
    fx.ta.write("x/IMG_1.CR3", RAW);
    fx.ta.write("x/IMG_1.jpg.xmp", EXACT);
    fx.ta.write("x/IMG_1.xmp", STEM);
    fx.scan().await;
    let (owner, exact, stem) = (
        fx.row_id(fx.a, "x/IMG_1.jpg").await,
        fx.row_id(fx.a, "x/IMG_1.jpg.xmp").await,
        fx.row_id(fx.a, "x/IMG_1.xmp").await,
    );
    assert_eq!(
        fx.links().await,
        links_of(&[
            ("a:x/IMG_1.CR3", "a:x/IMG_1.xmp", "stem"),
            ("a:x/IMG_1.jpg", "a:x/IMG_1.jpg.xmp", "exact"),
            ("a:x/IMG_1.jpg", "a:x/IMG_1.xmp", "stem"),
        ])
    );
    let group = MoveGroup {
        owner: request(
            loc(fx.a, "x/IMG_1.jpg"),
            loc(fx.a, "y/IMG_1.jpg"),
            ConflictPolicy::Smart,
        ),
        sidecars: vec![
            sidecar(&fx, "IMG_1.jpg.xmp", LinkKind::Exact, SidecarAction::Move),
            sidecar(&fx, "IMG_1.xmp", LinkKind::Stem, SidecarAction::Copy),
        ],
    };

    let outcome = run(&fx, group).await;

    let SidecarOutcome::Copied { dst, file_id: copy } = outcome.sidecars[1].clone() else {
        panic!("the stem sidecar was not copied: {outcome:?}");
    };
    assert_eq!(dst, loc(fx.a, "y/IMG_1.xmp"));
    assert_ne!(copy, stem, "a copy is a new file");
    let expected = vec![
        moved(loc(fx.a, "y/IMG_1.jpg.xmp"), exact),
        outcome.sidecars[1].clone(),
    ];
    assert_eq!(outcome.sidecars, expected);
    assert_eq!(
        outcome.owner,
        owner_moved(loc(fx.a, "y/IMG_1.jpg"), false, PHOTO, owner)
    );
    assert_eq!(
        fx.present(fx.a, "x/IMG_1.xmp", STEM, "the original").await,
        stem
    );
    assert_eq!(
        fx.present(fx.a, "y/IMG_1.xmp", STEM, "the copy").await,
        copy
    );
    fx.present(fx.a, "x/IMG_1.CR3", RAW, "the owner that stays")
        .await;
    assert_eq!(
        fx.links().await,
        links_of(&[
            ("a:x/IMG_1.CR3", "a:x/IMG_1.xmp", "stem"),
            ("a:y/IMG_1.jpg", "a:y/IMG_1.jpg.xmp", "exact"),
            ("a:y/IMG_1.jpg", "a:y/IMG_1.xmp", "stem"),
        ])
    );
    fx.assert_journal(
        &[("move", "done"), ("move", "done"), ("copy", "done")],
        "members",
    )
    .await;
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn every_member_of_a_group_is_announced_to_the_hooks_in_order() {
    #[derive(Default)]
    struct Recorder(Mutex<Vec<AppliedOp>>);

    impl OpHook for Recorder {
        fn on_applied<'a>(
            &'a self,
            _tx: &'a mut SqliteConnection,
            op: &'a AppliedOp,
        ) -> BoxFuture<'a, Result<(), sqlx::Error>> {
            Box::pin(async move {
                self.0.lock().unwrap().push(op.clone());
                Ok(())
            })
        }
    }

    let recorder = Arc::new(Recorder::default());
    let hook = Arc::clone(&recorder);
    let fx = Fx::with(|ops, _| ops.with_hook(hook)).await;
    let ids = trio(&fx).await;

    run(
        &fx,
        trio_group(&fx, loc(fx.a, "y/IMG_1.jpg"), ConflictPolicy::Smart),
    )
    .await;

    let seen = recorder.0.lock().unwrap().clone();
    let expected: Vec<AppliedOp> = [
        (ids.owner, "x/IMG_1.jpg", "y/IMG_1.jpg"),
        (ids.exact, "x/IMG_1.jpg.xmp", "y/IMG_1.jpg.xmp"),
        (ids.stem, "x/IMG_1.xmp", "y/IMG_1.xmp"),
    ]
    .into_iter()
    .map(|(file_id, from, to)| AppliedOp::Moved {
        file_id,
        from: loc(fx.a, from),
        to: loc(fx.a, to),
    })
    .collect();
    assert_eq!(seen, expected);
}

// --- groups that are refused or only partly done -----------------------------------------------

#[tokio::test]
async fn a_malformed_group_is_refused_before_anything_is_touched() {
    let fx = Fx::new().await;
    trio(&fx).await;
    fx.ta.write("other/IMG_1.xmp", STEM);
    fx.scan().await;
    let before = world(&fx).await;
    let too_long = format!("y/{}.jpg", "a".repeat(249));
    let exact = || sidecar(&fx, "IMG_1.jpg.xmp", LinkKind::Exact, SidecarAction::Move);
    let group = |dst: &str, sidecars: Vec<SidecarItem>| MoveGroup {
        owner: request(
            loc(fx.a, "x/IMG_1.jpg"),
            loc(fx.a, dst),
            ConflictPolicy::Smart,
        ),
        sidecars,
    };
    let elsewhere = SidecarItem {
        file_id: None,
        src: loc(fx.a, "other/IMG_1.xmp"),
        kind: LinkKind::Stem,
        action: SidecarAction::Move,
    };
    let on_another_volume = SidecarItem {
        src: loc(fx.b, "x/IMG_1.xmp"),
        ..elsewhere.clone()
    };
    let the_owner = SidecarItem {
        src: loc(fx.a, "x/IMG_1.jpg"),
        ..elsewhere.clone()
    };
    let cases = [
        (
            "a sidecar in another folder",
            group("y/IMG_1.jpg", vec![elsewhere]),
            "destination_invalid",
        ),
        (
            "a sidecar on another volume",
            group("y/IMG_1.jpg", vec![on_another_volume]),
            "destination_invalid",
        ),
        (
            "a sidecar listed twice",
            group("y/IMG_1.jpg", vec![exact(), exact()]),
            "destination_invalid",
        ),
        (
            "the owner as its own sidecar",
            group("y/IMG_1.jpg", vec![the_owner]),
            "destination_invalid",
        ),
        (
            "a derived name that is too long",
            group(&too_long, vec![exact()]),
            "name_too_long",
        ),
        (
            "a destination in the trash",
            group(".apm-trash/b/IMG_1.jpg", vec![exact()]),
            "destination_invalid",
        ),
    ];

    for (label, malformed, code) in cases {
        let refused = fx.ops.move_group(malformed, &cx()).await;
        assert_eq!(refused.unwrap_err().code(), code, "{label}");
        assert_eq!(world(&fx).await, before, "{label}: something was touched");
    }
}

#[tokio::test]
async fn an_owner_that_cannot_move_stops_the_group_before_any_sidecar() {
    let fx = Fx::new().await;
    trio(&fx).await;
    let before = world(&fx).await;
    let stale = Expect {
        size: len(PHOTO) + 1,
        mtime: BASE_MTIME,
    };
    let changed = |mut group: MoveGroup| {
        group.owner = MoveRequest {
            expect: Some(stale),
            ..group.owner
        };
        group
    };
    let to = |volume, dst: &str| trio_group(&fx, loc(volume, dst), ConflictPolicy::Smart);

    let refused = fx
        .ops
        .move_group(changed(to(fx.a, "y/IMG_1.jpg")), &cx())
        .await;
    assert_eq!(refused.unwrap_err(), FsOpError::SourceChanged);
    let refused = fx.ops.move_group(to(9999, "y/IMG_1.jpg"), &cx()).await;
    assert_eq!(refused.unwrap_err(), FsOpError::UnknownVolume);
    assert_eq!(
        world(&fx).await,
        before,
        "the sidecars stayed with their owner"
    );

    fs::remove_file(fx.ta.path("x/IMG_1.jpg")).unwrap();
    let gone = fx.ops.move_group(to(fx.a, "y/IMG_1.jpg"), &cx()).await;
    assert_eq!(gone.unwrap_err(), FsOpError::SourceMissing);
    fx.present(fx.a, "x/IMG_1.jpg.xmp", EXACT, "the sidecar")
        .await;
    fx.present(fx.a, "x/IMG_1.xmp", STEM, "the sidecar").await;
    assert!(fx.journal().await.is_empty());
    assert!(
        !fx.ta.path("y").exists(),
        "no folder was made for a group that did not move"
    );
}

#[tokio::test]
async fn a_sidecar_that_fails_is_reported_and_the_group_goes_on() {
    let outside = common::scratch_dir();
    let precious = outside.path().join("precious.xmp");
    fs::write(&precious, b"not ours to move").unwrap();
    let fx = Fx::new().await;
    let ids = trio(&fx).await;
    fs::remove_file(fx.ta.path("x/IMG_1.jpg.xmp")).unwrap();
    fs::remove_file(fx.ta.path("x/IMG_1.xmp")).unwrap();
    symlink(&precious, fx.ta.path("x/IMG_1.xmp")).unwrap();

    let outcome = run(
        &fx,
        trio_group(&fx, loc(fx.a, "y/IMG_1.jpg"), ConflictPolicy::Smart),
    )
    .await;

    let expected = GroupOutcome {
        owner: owner_moved(loc(fx.a, "y/IMG_1.jpg"), false, PHOTO, ids.owner),
        sidecars: vec![
            SidecarOutcome::Failed(FsOpError::SourceMissing),
            SidecarOutcome::Failed(FsOpError::SymlinkRejected),
        ],
    };
    assert_eq!(outcome, expected);
    assert_eq!(fs::read(&precious).unwrap(), b"not ours to move");
    assert!(
        fs::symlink_metadata(fx.ta.path("x/IMG_1.xmp"))
            .unwrap()
            .is_symlink()
    );
    fx.assert_journal(&[("move", "done")], "only the owner")
        .await;
    assert_eq!(fx.links().await, Links::new(), "no link crosses folders");
    let summary = fx.scan().await;
    assert_eq!(
        summary.removed, 2,
        "the scan forgets the sidecars that are gone: {summary:?}"
    );
    assert_eq!(
        fx.paths(fx.a).await,
        BTreeSet::from(["y/IMG_1.jpg".to_owned()])
    );
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn a_group_cancelled_before_it_starts_moves_nothing() {
    let fx = Fx::new().await;
    trio(&fx).await;
    let before = world(&fx).await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let changes = Arc::new(RecordingChanges::new());
    let context = OpContext::new(None, cancel, Arc::new(|_| {}), changes);

    let group = trio_group(&fx, loc(fx.a, "y/IMG_1.jpg"), ConflictPolicy::Smart);
    let refused = fx.ops.move_group(group, &context).await;

    assert_eq!(refused.unwrap_err(), FsOpError::Cancelled);
    assert_eq!(world(&fx).await, before);
}

#[tokio::test]
async fn a_group_whose_owner_moved_is_finished_even_when_it_is_cancelled() {
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let journal_rows = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&journal_rows);
    let fx = Fx::with(|ops, _| {
        ops.with_failpoints(Failpoints::new(move |step| {
            // The second journal row is the first sidecar's: the owner has landed.
            if step == AfterJournal && counted.fetch_add(1, Ordering::SeqCst) == 1 {
                trigger.cancel();
            }
            FailAction::Continue
        }))
    })
    .await;
    let ids = trio(&fx).await;
    let changes = Arc::new(RecordingChanges::new());
    let context = OpContext::new(None, cancel.clone(), Arc::new(|_| {}), changes);

    let group = trio_group(&fx, loc(fx.a, "y/IMG_1.jpg"), ConflictPolicy::Smart);
    let outcome = fx.ops.move_group(group, &context).await.unwrap();

    assert!(cancel.is_cancelled());
    assert_eq!(journal_rows.load(Ordering::SeqCst), 3);
    let expected = GroupOutcome {
        owner: owner_moved(loc(fx.a, "y/IMG_1.jpg"), false, PHOTO, ids.owner),
        sidecars: vec![
            moved(loc(fx.a, "y/IMG_1.jpg.xmp"), ids.exact),
            moved(loc(fx.a, "y/IMG_1.xmp"), ids.stem),
        ],
    };
    assert_eq!(outcome, expected, "no sidecar is left behind its owner");
    fx.assert_in_sync().await;
}

// --- between volumes ---------------------------------------------------------------------------

#[tokio::test]
async fn a_group_between_volumes_copies_every_member_and_keeps_the_links() {
    let fx = Fx::with(|ops, trees| ops.with_mover(Arc::new(SplitMover::new(&trees)))).await;
    let ids = trio(&fx).await;
    let census = fx.content_census();

    let group = trio_group(&fx, loc(fx.b, "y/IMG_1.jpg"), ConflictPolicy::Smart);
    let outcome = run(&fx, group).await;

    let expected = GroupOutcome {
        owner: MoveOutcome::Moved {
            dst: loc(fx.b, "y/IMG_1.jpg"),
            renamed: false,
            cross_device: true,
            bytes: len(PHOTO),
            file_id: ids.owner,
        },
        sidecars: vec![
            moved(loc(fx.b, "y/IMG_1.jpg.xmp"), ids.exact),
            moved(loc(fx.b, "y/IMG_1.xmp"), ids.stem),
        ],
    };
    assert_eq!(outcome, expected);
    for (path, content, id) in [
        ("y/IMG_1.jpg", PHOTO, ids.owner),
        ("y/IMG_1.jpg.xmp", EXACT, ids.exact),
        ("y/IMG_1.xmp", STEM, ids.stem),
    ] {
        assert_eq!(
            fx.present(fx.b, path, content, "arrived").await,
            id,
            "{path}"
        );
        let source = format!("x/{}", path.trim_start_matches("y/"));
        fx.gone(fx.a, &source, "left").await;
        assert_eq!(fx.hash_of(id).await, recorded_hash(path, content), "{path}");
    }
    assert_eq!(
        fx.links().await,
        links_of(&[
            ("b:y/IMG_1.jpg", "b:y/IMG_1.jpg.xmp", "exact"),
            ("b:y/IMG_1.jpg", "b:y/IMG_1.xmp", "stem"),
        ])
    );
    fx.assert_journal(&[("move", "done"); 3], "one row per member")
        .await;
    assert_eq!(fx.content_census(), census);
    assert_eq!(fx.leftover_parts(), Vec::<String>::new());
    fx.assert_in_sync().await;
}

// --- a group that is killed half way -----------------------------------------------------------

/// A group is three transfers, each with its own journal row and index transaction, so a crash in
/// one member leaves the earlier members done and the later ones untouched, and recovery settles
/// the member that was running (finishes it or undoes it) without looking at the others. What is
/// left is a file whose sidecars are somewhere else: the links across folders are dropped and
/// the ones that are in one folder again are kept.
///
/// `member` crashed at `step`; `nth` counts how often the step is reached over the whole group.
async fn crash_a_member(split: bool, step: Step, member: usize) {
    let reached = split || matches!(step, AfterJournal | AfterFinalRename | BeforeIndexTx);
    let undone = match step {
        AfterJournal => true,
        AfterPartCreate | AfterCopy | AfterFsync | AfterVerify => split,
        AfterFinalRename | BeforeUnlink | BeforeIndexTx => false,
    };
    let volumes = if split {
        "between volumes"
    } else {
        "in one volume"
    };
    let label = format!("{step:?} in member {member} ({volumes})");
    println!("crash at {label}");
    let trap = Trap::default();
    let fx = crash_fx(&trap, split).await;
    let ids = trio(&fx).await;
    let census = fx.content_census();
    let target = if split { fx.b } else { fx.a };
    trap.arm(step, member + 1);

    let group = trio_group(&fx, loc(target, "y/IMG_1.jpg"), ConflictPolicy::Smart);
    let result = fx.ops.move_group(group, &cx()).await;

    if !reached {
        assert!(result.is_ok(), "{label}: {result:?}");
        assert!(
            trap.armed(),
            "{label}: the group reached a step it has no use for"
        );
        fx.assert_in_sync_for(&label).await;
        return;
    }
    assert_eq!(result, Err(FsOpError::Aborted), "{label}");
    assert!(!trap.armed(), "{label}: the group never reached the step");

    let report = fx.restarted().reconcile().await.unwrap();

    let settled = if undone { (0, 1) } else { (1, 0) };
    assert_eq!(
        (report.completed, report.abandoned),
        settled,
        "{label}: {report:?}"
    );
    assert_eq!(report.still_pending, 0, "{label}: {report:?}");
    assert!(
        report.errors.is_empty() && report.rescan.is_empty(),
        "{label}: {report:?}"
    );
    let part_made = split && undone && step != AfterJournal;
    assert_eq!(
        report.parts_removed,
        usize::from(part_made),
        "{label}: {report:?}"
    );
    let has_moved = |index: usize| index < member || (index == member && !undone);
    let members = [
        ("x/IMG_1.jpg", "y/IMG_1.jpg", PHOTO, ids.owner),
        ("x/IMG_1.jpg.xmp", "y/IMG_1.jpg.xmp", EXACT, ids.exact),
        ("x/IMG_1.xmp", "y/IMG_1.xmp", STEM, ids.stem),
    ];
    let place = |index: usize| {
        let (from, to, ..) = members[index];
        if has_moved(index) {
            format!("{}:{to}", fx.label(target))
        } else {
            format!("a:{from}")
        }
    };
    for (index, (from, to, content, id)) in members.iter().enumerate() {
        let (at, other, other_path) = if has_moved(index) {
            (target, fx.a, *from)
        } else {
            (fx.a, target, *to)
        };
        let path = if has_moved(index) { *to } else { *from };
        assert_eq!(
            fx.present(at, path, content, &label).await,
            *id,
            "{label}: row of {path}"
        );
        fx.gone(other, other_path, &label).await;
        if split && has_moved(index) {
            assert_eq!(fx.hash_of(*id).await, recorded_hash(to, content), "{label}");
        }
    }
    let mut journal = vec![("move", "done"); member];
    journal.push(("move", if undone { "abandoned" } else { "done" }));
    fx.assert_journal(&journal, &label).await;

    // A link survives only when its two files are in the same folder.
    let mut linked = Links::new();
    for (index, kind) in [(1, "exact"), (2, "stem")] {
        if has_moved(0) == has_moved(index) {
            linked.insert((place(0), place(index), kind.to_owned()));
        }
    }
    assert_eq!(
        fx.links().await,
        linked,
        "{label}: the links straight after recovery"
    );

    recover(&fx, &label, &census, member == 0 && undone).await;
    assert_eq!(fx.links().await, linked, "{label}: the links after a scan");
}

async fn crash_a_group_at_every_step(split: bool) {
    for step in ALL_STEPS {
        let reached = split || matches!(step, AfterJournal | AfterFinalRename | BeforeIndexTx);
        for member in 0..if reached { 3 } else { 1 } {
            crash_a_member(split, step, member).await;
        }
    }
}

#[tokio::test]
async fn a_group_in_one_volume_survives_a_crash_in_any_member_at_any_step() {
    crash_a_group_at_every_step(false).await;
}

#[tokio::test]
async fn a_group_between_volumes_survives_a_crash_in_any_member_at_any_step() {
    crash_a_group_at_every_step(true).await;
}

#[tokio::test]
async fn a_crash_between_two_groups_loses_nothing_and_the_second_group_still_moves() {
    let trap = Trap::default();
    let fx = crash_fx(&trap, false).await;
    let first = trio(&fx).await;
    fx.ta.write("x/IMG_2.jpg", OTHER);
    fx.ta.write("x/IMG_2.xmp", STRANGER);
    fx.scan().await;
    let census = fx.content_census();
    trap.arm(AfterFinalRename, 2);

    let crashed = fx
        .ops
        .move_group(
            trio_group(&fx, loc(fx.a, "y/IMG_1.jpg"), ConflictPolicy::Smart),
            &cx(),
        )
        .await;
    assert_eq!(crashed, Err(FsOpError::Aborted));
    recover(&fx, "after the crash", &census, false).await;

    let second = MoveGroup {
        owner: request(
            loc(fx.a, "x/IMG_2.jpg"),
            loc(fx.a, "y/IMG_2.jpg"),
            ConflictPolicy::Smart,
        ),
        sidecars: vec![sidecar(
            &fx,
            "IMG_2.xmp",
            LinkKind::Stem,
            SidecarAction::Move,
        )],
    };
    let outcome = fx.restarted().move_group(second, &cx()).await.unwrap();

    assert!(
        matches!(outcome.owner, MoveOutcome::Moved { .. }),
        "{outcome:?}"
    );
    assert!(
        matches!(outcome.sidecars[0], SidecarOutcome::Moved { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        fx.present(fx.a, "y/IMG_1.jpg.xmp", EXACT, "recovered")
            .await,
        first.exact
    );
    fx.present(
        fx.a,
        "x/IMG_1.xmp",
        STEM,
        "still waiting for its owner's folder",
    )
    .await;
    fx.assert_in_sync().await;
}
