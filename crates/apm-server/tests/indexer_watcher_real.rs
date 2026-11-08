//! Opt-in native watcher coverage on macOS and Linux, using real jobs and scratch volumes.
mod common;

use common::watcher::{Harness, eventually};

#[tokio::test]
#[ignore = "requires native filesystem notifications; run explicitly on macOS and Linux"]
async fn watcher_real_create_modify_delete() {
    let mut h = Harness::new().await;
    h.scan().await;
    let watches = h.start_native().await;
    h.tree.write("a.txt", b"a");
    h.wait_files(&[("", "a.txt", 1)]).await;
    h.tree.write("a.txt", b"changed");
    h.wait_files(&[("", "a.txt", 7)]).await;
    std::fs::remove_file(h.tree.path("a.txt")).unwrap();
    h.wait_files(&[]).await;
    h.stop().await;
    assert!(watches.paths().is_empty());
}

#[tokio::test]
#[ignore = "requires native filesystem notifications; run explicitly on macOS and Linux"]
async fn watcher_real_directory_rename_then_write_at_new_path() {
    let mut h = Harness::new().await;
    h.tree.write("old/nested/a.txt", b"a");
    h.scan().await;
    let watches = h.start_native().await;
    if cfg!(target_os = "linux") {
        eventually(|| h.app.state.watcher.status().watched_dirs == 3).await;
    }
    std::fs::rename(h.tree.path("old"), h.tree.path("new")).unwrap();
    h.wait_files(&[("new/nested", "a.txt", 1)]).await;
    if cfg!(target_os = "linux") {
        eventually(|| watches.paths().contains(&h.tree.path("new/nested"))).await;
        assert!(
            !watches
                .paths()
                .iter()
                .any(|path| path.starts_with(h.tree.path("old")))
        );
    }
    h.tree.write("new/nested/b.txt", b"bb");
    h.wait_files(&[("new/nested", "a.txt", 1), ("new/nested", "b.txt", 2)])
        .await;
    std::fs::remove_dir_all(h.tree.path("new")).unwrap();
    h.wait_files(&[]).await;
    h.stop().await;
    assert!(watches.paths().is_empty());
}
