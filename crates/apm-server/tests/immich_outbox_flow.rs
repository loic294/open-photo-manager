//! The outbox of file operations (`immich/outbox/`) from the file operation to the asset in
//! Immich: a move, a trash, a copy or a restore of the application is mirrored into a fake Immich
//! over real directories, through the real `FsOps` (with the outbox hook), the real task engine
//! and the real client. See `immich_outbox_faults.rs` for what goes wrong and
//! `immich_outbox_api.rs` for the endpoints.
//!
//! A pass is started by the test (`Fixture::pass`) and the rows are made due again by the test
//! (`Fixture::make_due`), which is all the waits of the real scheduler come to. Every test checks
//! that the fake recorded no forced delete of an asset that was still online.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod immich_support;

use immich_support::{Fixture, delete_of, first_pass, next_pass, one_photo};

#[tokio::test]
async fn a_move_inside_a_library_carries_what_immich_knew_to_the_new_asset() {
    let (fx, photos, old) = one_photo().await;
    fx.fake.add_album("Holiday", &[&old]);
    let family = fx.fake.add_tag("family");
    fx.fake.tag_asset(&family, &old);
    fx.fake.update_asset(&old, |asset| {
        asset.is_favorite = true;
        asset.rating = Some(4);
        asset.description = Some("The lake".to_owned());
    });

    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    let row = fx.only_row().await;
    assert_eq!(row.old_path().as_deref(), Some("2020/a.jpg"));
    assert_eq!(row.new_path().as_deref(), Some("2021/a.jpg"));
    assert_eq!(fx.fake.calls_to("DELETE", "/api/assets").len(), 0);

    let report = first_pass(&fx).await;
    assert_eq!(report["scans"], 1, "{report}");
    assert_eq!(fx.scans_asked(), std::slice::from_ref(&photos.library_id));
    // The fake imported the new path and lost the old file; nothing is deleted before it is checked.
    assert!(fx.fake.has_asset(&old));
    assert!(fx.fake.asset(&old).is_offline);
    assert!(fx.deletes().is_empty());
    let waiting = fx.only_row().await;
    assert_eq!(waiting.stage, 1);
    assert_eq!(waiting.asset_id.as_deref(), Some(old.as_str()));

    let report = next_pass(&fx).await;
    assert_eq!(report["carried"], 1, "{report}");
    assert_eq!(report["deleted"], 1, "{report}");

    let new = fx.fake.asset_at(&photos.immich_path("2021/a.jpg"));
    assert_ne!(new.id, old);
    assert!(new.is_favorite);
    assert_eq!(new.rating, Some(4));
    assert_eq!(new.description.as_deref(), Some("The lake"));
    assert_eq!(fx.fake.albums_of(&new.id), ["Holiday"]);
    assert_eq!(fx.fake.tags_of(&new.id), ["family"]);
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    assert!(!fx.fake.has_asset(&old));
    assert_eq!(fx.row_count().await, 0);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn what_immich_knew_is_read_before_the_scan_and_the_old_asset_again_before_the_delete() {
    let (fx, photos, old) = one_photo().await;
    fx.fake.add_album("Holiday", &[&old]);
    fx.fake.clear_calls();
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    first_pass(&fx).await;
    next_pass(&fx).await;

    let calls = fx.fake.sequence();
    let position = |call: &str| calls.iter().position(|seen| seen == call);
    let snapshot = position("GET /api/albums").expect("the albums were read");
    let scan = calls
        .iter()
        .position(|seen| seen.ends_with("/scan"))
        .expect("the scan was asked");
    assert!(
        snapshot < scan,
        "the scan turns the asset offline: {calls:#?}"
    );

    let delete = position("DELETE /api/assets").expect("the old asset was deleted");
    let last_carry = calls[..delete]
        .iter()
        .rposition(|seen| seen.starts_with("PUT /api/"))
        .expect("the state was carried");
    let fresh = calls[..delete]
        .iter()
        .rposition(|seen| *seen == format!("GET /api/assets/{old}"))
        .expect("the old asset was read before the delete");
    assert!(
        fresh > last_carry,
        "the old asset is read again after the carry-over: {calls:#?}"
    );
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_move_to_a_volume_that_is_not_mapped_removes_the_old_asset_without_a_carry() {
    let fx = Fixture::connected().await;
    let photos = fx.site("photos").await;
    let loose = fx.site("loose").await;
    fx.map_sites(&[&photos]).await;
    let old = fx.seed_one(&photos, "2020/a.jpg").await;
    fx.sync_ok(false).await;
    fx.fake.add_album("Holiday", &[&old]);
    fx.fake.clear_calls();

    fx.move_file((&photos, "2020/a.jpg"), (&loose, "a.jpg"))
        .await;
    let report = first_pass(&fx).await;
    assert_eq!(report["scans"], 1, "{report}");
    assert_eq!(fx.scans_asked(), std::slice::from_ref(&photos.library_id));
    assert_eq!(
        fx.fake.count("GET", "/api/albums"),
        0,
        "there is no new asset to carry anything to"
    );

    let report = next_pass(&fx).await;
    assert_eq!(report["deleted"], 1, "{report}");
    assert_eq!(report["carried"], 0, "{report}");
    assert_eq!(
        fx.carries(),
        0,
        "nothing is carried to a place Immich does not see"
    );
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    assert!(fx.fake.assets_at(&loose.immich_path("a.jpg")).is_empty());
    assert_eq!(fx.row_count().await, 0);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_move_into_a_folder_the_library_excludes_waits_ten_minutes_for_an_asset_that_never_comes()
{
    let (fx, photos, old) = one_photo().await;
    fx.fake.exclude(&photos.library_id, "**/private/**");
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "private/a.jpg"))
        .await;
    first_pass(&fx).await;
    assert!(fx.fake.asset(&old).is_offline);

    let report = next_pass(&fx).await;
    assert_eq!(report["waiting"], 1, "{report}");
    assert!(fx.deletes().is_empty());
    let row = fx.only_row().await;
    assert!(row.offline_seen_at.is_some(), "{row:?}");
    assert!(row.is_pending());

    let report = next_pass(&fx).await;
    assert_eq!(
        report["waiting"], 1,
        "still inside the grace period: {report}"
    );
    assert!(fx.deletes().is_empty());

    fx.age_offline_seen(700).await;
    let report = next_pass(&fx).await;
    assert_eq!(report["deleted"], 1, "{report}");
    assert_eq!(report["carried"], 0, "{report}");
    assert_eq!(
        fx.carries(),
        0,
        "the new asset never came: nothing to carry to"
    );
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    assert!(
        fx.fake
            .assets_at(&photos.immich_path("private/a.jpg"))
            .is_empty()
    );
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_scan_that_never_runs_ends_in_attention_and_the_old_asset_is_never_deleted() {
    let (fx, photos, old) = one_photo().await;
    fx.fake.add_album("Holiday", &[&old]);
    fx.fake.hold_scans(true);
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    first_pass(&fx).await;
    assert_eq!(fx.scans_asked().len(), 1);
    assert!(!fx.fake.asset(&old).is_offline, "Immich has not scanned");

    for attempt in 1..=2 {
        let report = next_pass(&fx).await;
        assert_eq!(report["waiting"], 1, "attempt {attempt}: {report}");
        let row = fx.only_row().await;
        assert!(row.is_pending(), "{row:?}");
        assert_eq!(row.attempts, attempt);
    }

    // Two hours after the request, and only after a few tries, a person is asked.
    fx.age_rows(7_300).await;
    let report = next_pass(&fx).await;
    assert_eq!(report["attention"], 1, "{report}");
    let row = fx.only_row().await;
    assert!(row.needs_attention(), "{row:?}");
    assert_eq!(row.last_error.as_deref(), Some("not_offline"));
    assert!(fx.deletes().is_empty());
    assert!(!fx.fake.asset(&old).is_offline);

    // Immich finally scans; a person presses Retry and the change completes.
    assert_eq!(fx.fake.run_held_scans(), 1);
    assert_eq!(fx.retry_all().await, 1);
    let report = first_pass(&fx).await;
    assert_eq!(report["carried"], 1, "{report}");
    assert_eq!(report["deleted"], 1, "{report}");
    let new = fx.fake.asset_at(&photos.immich_path("2021/a.jpg"));
    assert_eq!(fx.fake.albums_of(&new.id), ["Holiday"]);
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_file_that_comes_back_before_the_check_supersedes_the_change() {
    let (fx, photos, old) = one_photo().await;
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    first_pass(&fx).await;
    assert!(fx.fake.asset(&old).is_offline);

    photos.tree.write("2020/a.jpg", b"put back from a backup");
    let report = next_pass(&fx).await;
    assert_eq!(report["superseded"], 1, "{report}");
    assert!(fx.deletes().is_empty());
    assert_eq!(fx.row_count().await, 0);

    // Immich's own next scan brings the asset back, with everything it had.
    fx.fake.scan_library(&photos.library_id);
    assert!(!fx.fake.asset(&old).is_offline);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn an_old_asset_that_is_online_again_after_it_was_seen_offline_is_left_alone() {
    let (fx, photos, old) = one_photo().await;
    fx.fake.exclude(&photos.library_id, "**/private/**");
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "private/a.jpg"))
        .await;
    first_pass(&fx).await;
    let report = next_pass(&fx).await;
    assert_eq!(report["waiting"], 1, "{report}");
    assert!(fx.only_row().await.offline_seen_at.is_some());

    fx.fake.update_asset(&old, |asset| {
        asset.is_offline = false;
        asset.is_trashed = false;
    });
    let report = next_pass(&fx).await;
    assert_eq!(report["superseded"], 1, "{report}");
    assert!(fx.deletes().is_empty());
    assert_eq!(fx.row_count().await, 0);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_trashed_file_has_its_asset_removed_once_the_scan_saw_it_leave() {
    let (fx, photos, old) = one_photo().await;
    fx.fake.add_album("Holiday", &[&old]);
    fx.fake.clear_calls();

    fx.trash_file(&photos, "2020/a.jpg").await;
    let row = fx.only_row().await;
    assert_eq!(row.old_path().as_deref(), Some("2020/a.jpg"));
    assert_eq!(row.new_path(), None);

    let report = first_pass(&fx).await;
    assert_eq!(report["scans"], 1, "{report}");
    assert!(
        fx.fake.asset(&old).is_offline,
        "the trash folder is not scanned"
    );
    assert_eq!(
        fx.fake.count("GET", "/api/albums"),
        0,
        "nothing to carry to"
    );

    let report = next_pass(&fx).await;
    assert_eq!(report["deleted"], 1, "{report}");
    assert_eq!(report["carried"], 0, "{report}");
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    assert_eq!(fx.row_count().await, 0);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_copy_only_needs_the_scan() {
    let (fx, photos, old) = one_photo().await;
    fx.copy_file((&photos, "2020/a.jpg"), (&photos, "2020/b.jpg"))
        .await;
    let row = fx.only_row().await;
    assert_eq!(row.old_path(), None);
    assert_eq!(row.new_path().as_deref(), Some("2020/b.jpg"));
    fx.fake.clear_calls();

    let report = first_pass(&fx).await;
    assert_eq!(report["scans"], 1, "{report}");
    assert_eq!(report["scanned"], 1, "{report}");
    assert_eq!(fx.row_count().await, 0);
    assert!(
        fx.fake
            .calls()
            .iter()
            .all(|call| !call.path.starts_with("/api/assets")),
        "no asset is read or changed: {:#?}",
        fx.fake.sequence()
    );
    assert!(
        !fx.fake
            .asset_at(&photos.immich_path("2020/b.jpg"))
            .is_offline
    );
    assert!(fx.fake.has_asset(&old));
    assert!(!fx.fake.asset(&old).is_offline);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_restore_from_the_trash_lets_immich_scan_the_file_back_in() {
    let (fx, photos, old) = one_photo().await;
    let item = fx.trash_file(&photos, "2020/a.jpg").await;
    first_pass(&fx).await;
    next_pass(&fx).await;
    assert!(!fx.fake.has_asset(&old));

    fx.restore(item).await;
    let row = fx.only_row().await;
    assert_eq!(row.old_path(), None);
    assert_eq!(row.new_path().as_deref(), Some("2020/a.jpg"));
    // The earlier pass asked for a scan a moment ago: a person's Retry lets this one ask again.
    fx.retry_all().await;
    let report = first_pass(&fx).await;
    assert_eq!(report["scanned"], 1, "{report}");
    let back = fx.fake.asset_at(&photos.immich_path("2020/a.jpg"));
    assert_ne!(back.id, old);
    assert!(!back.is_offline);
    assert_eq!(fx.row_count().await, 0);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_trash_followed_by_a_restore_before_the_pass_cancels_out() {
    let (fx, photos, old) = one_photo().await;
    fx.fake.clear_calls();
    let item = fx.trash_file(&photos, "2020/a.jpg").await;
    assert_eq!(fx.row_count().await, 1);
    fx.restore(item).await;
    assert_eq!(
        fx.row_count().await,
        0,
        "the file is where Immich has it: nothing to reflect"
    );

    assert!(
        fx.app
            .state
            .immich_outbox
            .submit_now()
            .await
            .unwrap()
            .is_none()
    );
    assert!(fx.fake.calls().is_empty(), "{:#?}", fx.fake.sequence());
    assert!(!fx.fake.asset(&old).is_offline);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_restore_after_the_scan_was_asked_supersedes_the_trash_and_nothing_is_deleted() {
    let (fx, photos, old) = one_photo().await;
    let item = fx.trash_file(&photos, "2020/a.jpg").await;
    first_pass(&fx).await;
    assert!(
        fx.fake.asset(&old).is_offline,
        "the scan saw the file leave"
    );

    // The file is back before the trash was checked: a second change, the first one is waiting.
    fx.restore(item).await;
    assert_eq!(fx.row_count().await, 2);

    let report = next_pass(&fx).await;
    assert_eq!(report["superseded"], 1, "the file is back: {report}");
    assert_eq!(
        report["waiting"], 1,
        "the scan of a moment ago predates the restore: {report}"
    );
    assert!(fx.deletes().is_empty());

    // A person's Retry asks the scan again, which brings the asset back online.
    fx.retry_all().await;
    let report = first_pass(&fx).await;
    assert_eq!(report["scanned"], 1, "{report}");
    assert!(fx.fake.has_asset(&old));
    assert!(!fx.fake.asset(&old).is_offline);
    assert!(fx.deletes().is_empty());
    assert_eq!(fx.row_count().await, 0);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_move_between_two_libraries_asks_both_scans_at_once_and_carries_to_the_other_library() {
    let fx = Fixture::connected().await;
    let photos = fx.site("photos").await;
    let archive = fx.site("archive").await;
    fx.map_sites(&[&photos, &archive]).await;
    let old = fx.seed_one(&photos, "2020/a.jpg").await;
    fx.sync_ok(false).await;
    fx.fake.add_album("Holiday", &[&old]);
    fx.fake.update_asset(&old, |asset| asset.is_favorite = true);

    fx.move_file((&photos, "2020/a.jpg"), (&archive, "2020/a.jpg"))
        .await;
    let report = first_pass(&fx).await;
    assert_eq!(report["scans"], 2, "{report}");
    let mut asked = fx.scans_asked();
    asked.sort();
    let mut both = vec![photos.library_id.clone(), archive.library_id.clone()];
    both.sort();
    assert_eq!(asked, both);

    let report = next_pass(&fx).await;
    assert_eq!(report["carried"], 1, "{report}");
    assert_eq!(report["deleted"], 1, "{report}");
    let new = fx.fake.asset_at(&archive.immich_path("2020/a.jpg"));
    assert!(new.is_favorite);
    assert_eq!(fx.fake.albums_of(&new.id), ["Holiday"]);
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn two_moves_before_the_pass_are_one_change_from_the_first_place_to_the_last() {
    let (fx, photos, old) = one_photo().await;
    fx.fake.add_album("Holiday", &[&old]);
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    fx.move_file((&photos, "2021/a.jpg"), (&photos, "2022/a.jpg"))
        .await;
    let row = fx.only_row().await;
    assert_eq!(row.old_path().as_deref(), Some("2020/a.jpg"));
    assert_eq!(row.new_path().as_deref(), Some("2022/a.jpg"));

    first_pass(&fx).await;
    let report = next_pass(&fx).await;
    assert_eq!(report["carried"], 1, "{report}");
    assert_eq!(report["deleted"], 1, "{report}");
    let new = fx.fake.asset_at(&photos.immich_path("2022/a.jpg"));
    assert_eq!(fx.fake.albums_of(&new.id), ["Holiday"]);
    assert!(
        fx.fake
            .assets_at(&photos.immich_path("2021/a.jpg"))
            .is_empty(),
        "Immich never saw the file in between"
    );
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_move_and_the_move_back_cancel_out() {
    let (fx, photos, old) = one_photo().await;
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    fx.move_file((&photos, "2021/a.jpg"), (&photos, "2020/a.jpg"))
        .await;
    assert_eq!(fx.row_count().await, 0, "the file is where Immich has it");
    assert!(
        fx.app
            .state
            .immich_outbox
            .submit_now()
            .await
            .unwrap()
            .is_none()
    );
    assert!(fx.fake.has_asset(&old));
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_move_after_the_scan_was_asked_is_a_second_change_and_the_chain_still_ends_well() {
    let (fx, photos, first) = one_photo().await;
    fx.fake.add_album("Holiday", &[&first]);
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    first_pass(&fx).await;
    let second = fx.fake.asset_at(&photos.immich_path("2021/a.jpg")).id;

    // The file moves on before the first change was checked: a new row, the first one is stage 1.
    fx.move_file((&photos, "2021/a.jpg"), (&photos, "2022/a.jpg"))
        .await;
    assert_eq!(fx.row_count().await, 2);

    // The first change is settled and carries the album to the intermediate asset; the second one
    // waits for the throttle, since the scan that was asked a moment ago predates it.
    let report = next_pass(&fx).await;
    assert_eq!(report["deleted"], 1, "{report}");
    assert_eq!(report["waiting"], 1, "{report}");
    assert!(!fx.fake.has_asset(&first));
    assert!(fx.fake.has_asset(&second));
    assert_eq!(fx.fake.albums_of(&second), ["Holiday"]);
    assert_eq!(fx.deletes(), [delete_of(&first)]);

    // A person presses Retry: the scan is asked again, and what the intermediate asset now holds
    // is what reaches the last one.
    fx.retry_all().await;
    let report = first_pass(&fx).await;
    assert_eq!(report["scans"], 1, "{report}");
    let report = next_pass(&fx).await;
    assert_eq!(report["deleted"], 1, "{report}");
    let last = fx.fake.asset_at(&photos.immich_path("2022/a.jpg"));
    assert_eq!(fx.fake.albums_of(&last.id), ["Holiday"]);
    assert!(!fx.fake.has_asset(&second));
    assert_eq!(fx.deletes(), [delete_of(&first), delete_of(&second)]);
    assert_eq!(fx.row_count().await, 0);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn an_asset_that_followed_the_file_by_itself_is_not_deleted() {
    let (fx, photos, old) = one_photo().await;
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    first_pass(&fx).await;
    // Immich tracked the file: the old asset is live at the new path and the import is gone.
    let imported = fx.fake.asset_at(&photos.immich_path("2021/a.jpg")).id;
    fx.fake.remove_asset(&imported);
    fx.fake.update_asset(&old, |asset| {
        asset.original_path = photos.immich_path("2021/a.jpg");
        asset.is_offline = false;
        asset.is_trashed = false;
    });

    let report = next_pass(&fx).await;
    assert_eq!(report["superseded"], 1, "{report}");
    assert!(fx.deletes().is_empty());
    assert!(fx.fake.has_asset(&old));
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn an_asset_at_another_path_than_the_file_is_not_ours_to_delete() {
    let (fx, photos, old) = one_photo().await;
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    first_pass(&fx).await;
    fx.fake.update_asset(&old, |asset| {
        asset.original_path = photos.immich_path("elsewhere/x.jpg");
    });

    let report = next_pass(&fx).await;
    assert_eq!(report["attention"], 1, "{report}");
    let row = fx.only_row().await;
    assert!(row.needs_attention());
    assert_eq!(row.last_error.as_deref(), Some("path_mismatch"));
    assert!(fx.deletes().is_empty());
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn an_asset_that_is_already_gone_ends_the_change_without_a_delete() {
    let (fx, photos, old) = one_photo().await;
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    first_pass(&fx).await;
    fx.fake.remove_asset(&old);

    let report = next_pass(&fx).await;
    assert_eq!(report["gone"], 1, "{report}");
    assert!(fx.deletes().is_empty());
    assert_eq!(fx.row_count().await, 0);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_new_asset_that_is_still_being_imported_is_waited_for_before_anything_is_deleted() {
    let (fx, photos, old) = one_photo().await;
    fx.fake.add_album("Holiday", &[&old]);
    fx.fake.slow_imports(true);
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    first_pass(&fx).await;

    let report = next_pass(&fx).await;
    assert_eq!(report["waiting"], 1, "{report}");
    assert!(fx.deletes().is_empty());
    assert_eq!(
        fx.carries(),
        0,
        "nothing is carried while the new asset is imported"
    );

    fx.fake.finish_imports();
    let report = next_pass(&fx).await;
    assert_eq!(report["carried"], 1, "{report}");
    assert_eq!(report["deleted"], 1, "{report}");
    let new = fx.fake.asset_at(&photos.immich_path("2021/a.jpg"));
    assert_eq!(fx.fake.albums_of(&new.id), ["Holiday"]);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_move_of_a_file_immich_never_imported_only_needs_the_scan() {
    let fx = Fixture::connected().await;
    let photos = fx.site("photos").await;
    fx.map_sites(&[&photos]).await;
    photos.tree.write("2020/a.jpg", b"never scanned by Immich");
    fx.index().await;
    fx.sync_ok(false).await;

    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    let report = first_pass(&fx).await;
    assert_eq!(report["scanned"], 1, "{report}");
    assert_eq!(report["scans"], 1, "{report}");
    assert!(fx.deletes().is_empty());
    assert_eq!(fx.row_count().await, 0);
    assert!(
        !fx.fake
            .asset_at(&photos.immich_path("2021/a.jpg"))
            .is_offline
    );
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn a_move_takes_the_sidecar_along_but_only_the_photo_is_queued() {
    let (fx, photos, old) = one_photo().await;
    photos.tree.write("2020/a.xmp", b"<x:xmpmeta/>");
    fx.index().await;

    fx.move_with_sidecar(
        (&photos, "2020/a.jpg"),
        "2020/a.xmp",
        (&photos, "2021/a.jpg"),
    )
    .await;
    assert!(
        photos.tree.path("2021/a.xmp").exists(),
        "the sidecar went along"
    );
    let row = fx.only_row().await;
    assert_eq!(row.old_path().as_deref(), Some("2020/a.jpg"));
    assert_eq!(row.new_path().as_deref(), Some("2021/a.jpg"));

    first_pass(&fx).await;
    next_pass(&fx).await;
    assert_eq!(fx.deletes(), [delete_of(&old)]);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn many_moves_in_one_pass_share_one_scan_and_each_asset_keeps_its_own_state() {
    let fx = Fixture::connected().await;
    let photos = fx.site("photos").await;
    fx.map_sites(&[&photos]).await;
    let names: Vec<String> = (0..30).map(|n| format!("2020/p{n:02}.jpg")).collect();
    let rels: Vec<&str> = names.iter().map(String::as_str).collect();
    let olds = fx.seed(&photos, &rels).await;
    fx.sync_ok(false).await;
    for (n, id) in olds.iter().enumerate() {
        if n % 3 == 0 {
            fx.fake.update_asset(id, |asset| asset.is_favorite = true);
        }
        fx.fake
            .update_asset(id, |asset| asset.description = Some(format!("photo {n}")));
    }
    for (n, rel) in rels.iter().enumerate() {
        fx.move_file((&photos, rel), (&photos, &format!("2021/p{n:02}.jpg")))
            .await;
    }
    assert_eq!(fx.row_count().await, 30);

    let report = first_pass(&fx).await;
    assert_eq!(report["scans"], 1, "one scan covers every row: {report}");
    assert_eq!(fx.scans_asked().len(), 1);
    let report = next_pass(&fx).await;
    assert_eq!(report["deleted"], 30, "{report}");
    assert_eq!(report["carried"], 30, "{report}");

    for n in 0..30 {
        let new = fx
            .fake
            .asset_at(&photos.immich_path(&format!("2021/p{n:02}.jpg")));
        assert_eq!(new.is_favorite, n % 3 == 0, "photo {n}");
        assert_eq!(new.description, Some(format!("photo {n}")), "photo {n}");
    }
    let mut deleted: Vec<String> = fx
        .deletes()
        .iter()
        .map(|body| body["ids"][0].as_str().expect("one id").to_owned())
        .collect();
    deleted.sort();
    let mut expected = olds.clone();
    expected.sort();
    assert_eq!(deleted, expected);
    assert_eq!(fx.row_count().await, 0);
    fx.fake.assert_no_violations();
}
