//! Opt-in observations of a real mount. `APM_PROBE_DIR` must name a disposable directory.

mod common;

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirEntryExt, MetadataExt};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use apm_core::dto::ConflictPolicy;
use apm_core::mounts::{lookup, parse_mountinfo};
use apm_core::paths::RelPath;
use apm_server::config::Config;
use apm_server::fsops::{Loc, MoveRequest, OpContext, executor::MoveOutcome};
use apm_server::state::AppState;

#[tokio::test]
#[ignore = "requires APM_PROBE_DIR to point at a disposable real mount"]
async fn observes_real_mount_capabilities_and_file_operations() {
    let Some(probe_dir) = std::env::var_os("APM_PROBE_DIR").map(PathBuf::from) else {
        eprintln!("APM_PROBE_DIR is unset; skipping real mount observations");
        return;
    };
    assert!(probe_dir.is_dir(), "APM_PROBE_DIR must already exist");

    let work = common::scratch_dir();
    let mut config = Config::for_tests(work.path());
    config.volumes_dir = work.path().join("volumes");
    config.data_dir = probe_dir.join("data");
    fs::create_dir_all(&config.volumes_dir).unwrap();
    fs::create_dir_all(&config.data_dir).unwrap();
    std::os::unix::fs::symlink(&probe_dir, config.volumes_dir.join("probe")).unwrap();

    let state = AppState::new(config.clone()).await.unwrap();
    let volume = state
        .volumes
        .list()
        .into_iter()
        .find(|volume| volume.name == "probe")
        .unwrap();
    let capability = state.volumes.capabilities(volume.id).unwrap();
    let data_fs_type = fs::read("/proc/self/mountinfo").ok().and_then(|input| {
        lookup(
            &parse_mountinfo(&input),
            config.data_dir.as_os_str().as_bytes(),
        )
        .map(|mount| mount.fs_type.clone())
    });

    let marker = format!(
        ".apm-observation-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let marker_path = probe_dir.join(&marker);
    fs::write(&marker_path, b"probe").unwrap();
    let first = fs::metadata(&marker_path).unwrap();
    let second = fs::symlink_metadata(&marker_path).unwrap();
    let entry_inode = fs::read_dir(&probe_dir)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| entry.file_name().as_os_str() == PathBuf::from(&marker).as_os_str())
        .map(|entry| entry.ino())
        .unwrap();
    let d_ino_matches = entry_inode == first.ino();
    let inode_stable = first.ino() == second.ino() && first.dev() == second.dev();

    let directory_sync = fs::File::open(&probe_dir).and_then(|directory| directory.sync_all());
    let source = probe_dir.join("APM-Case.jpg");
    fs::write(&source, b"case-only rename probe").unwrap();
    let outcome = state
        .fsops
        .move_file(
            MoveRequest {
                file_id: None,
                src: Loc::new(volume.id, RelPath::new("APM-Case.jpg").unwrap()),
                dst: Loc::new(volume.id, RelPath::new("apm-case.jpg").unwrap()),
                policy: ConflictPolicy::Smart,
                expect: None,
            },
            &OpContext::detached(),
        )
        .await;
    let case_rename_succeeded = matches!(outcome, Ok(MoveOutcome::Moved { .. }));
    let case_destination = probe_dir.join("apm-case.jpg");
    let case_source = probe_dir.join("APM-Case.jpg");
    for path in [&case_destination, &case_source] {
        if let Err(error) = fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            panic!("could not clean up case-rename probe: {error}");
        }
    }
    fs::remove_file(&marker_path).unwrap();

    println!(
        "volume_fs_type={:?} data_fs_type={:?} case_insensitive={:?} inode_reliable={:?} \
         inode_stable={inode_stable} d_ino_matches={d_ino_matches} directory_fsync={directory_sync:?} \
         case_only_rename_succeeded={case_rename_succeeded} journal_mode={}",
        capability.fs_type,
        data_fs_type,
        capability.case_insensitive,
        capability.inode_reliable,
        state.db.journal_mode()
    );

    assert!(capability.case_insensitive.is_some());
    assert!(capability.inode_reliable.is_some());
    assert!(!marker_path.exists());
    assert!(!case_destination.exists() && !case_source.exists());

    state.shutdown().await;
    fs::remove_dir_all(&config.data_dir).unwrap();
}
