//! The `FsOps` hook that queues what the file operations do to photos and videos for Immich.
//!
//! It runs inside the transaction that makes an operation final (see `fsops::hooks`), so "the file
//! moved" and "the outbox row exists" commit or roll back together. That puts three rules on it:
//!
//! - **Fast**: the transaction holds the only write connection. One statement decides whether the
//!   change matters (Immich is connected and the old or the new volume is mapped) and fetches the
//!   row that is already waiting for the file; at most one more writes. No Immich call, no file
//!   access, nothing that waits.
//! - **Never in the way**: an error is returned ONLY when a statement fails, which rolls the
//!   operation back like any other storage failure. Everything else (not connected, an unmapped
//!   volume, a file that is not a photo or a video, a change that cancels the waiting one) is an
//!   `Ok` that does nothing.
//! - **Idempotent**: `reconcile` replays the hooks of an operation that was cut short; the same
//!   operation twice leaves one identical row.
//!
//! What is queued, by operation (only photos and videos, by the extension of the name):
//!
//! | operation | old place     | new place     | key of the file            |
//! |-----------|---------------|---------------|----------------------------|
//! | move      | where it was  | where it is   | the file's row (it moves)  |
//! | trash     | where it was  | none          | the file's row, else the trash item |
//! | copy      | none          | the copy      | the copy's row             |
//! | restore   | none          | the restored  | the restored row           |
//!
//! A rename that changes the kind (`a.jpg` to `a.txt`) counts only for the side that is a photo.
//! Not queued: sidecars, audio and other files; folders (a trashed or restored folder is a known
//! limit, see the module docs of `outbox`); a purge (the file left the volume long before); and
//! moves or copies INTO a junk path, which `fsops` does not report to hooks at all.

use apm_core::dto::FileKind;
use apm_core::kind::classify_name;
use futures_util::future::BoxFuture;
use sqlx::SqliteConnection;

use super::merge::{Effect, Place};
use super::signal::OutboxSignal;
use super::store::{self, Queued};
use crate::db::now_secs;
use crate::fsops::{AppliedOp, Loc, OpHook};

/// Register it with `FsOps::with_hook` before `reconcile_at_startup`.
#[derive(Clone)]
pub struct OutboxHook {
    signal: OutboxSignal,
}

impl OutboxHook {
    pub fn new(signal: OutboxSignal) -> Self {
        Self { signal }
    }
}

impl OpHook for OutboxHook {
    fn on_applied<'a>(
        &'a self,
        tx: &'a mut SqliteConnection,
        op: &'a AppliedOp,
    ) -> BoxFuture<'a, Result<(), sqlx::Error>> {
        Box::pin(async move {
            if let AppliedOp::Moved { from, to, .. } | AppliedOp::Copied { from, to, .. } = op {
                super::direct::confirm_sidecar(tx, from, to).await?;
            }
            if let AppliedOp::Moved { file_id, from, to } = op
                && super::direct::queue(tx, *file_id, from, to).await?
            {
                self.signal.enqueued();
                return Ok(());
            }
            let Some((key, effect)) = change_of(op) else {
                return Ok(());
            };
            match store::queue(tx, key, &effect, now_secs()).await? {
                Queued::Inserted | Queued::Merged => self.signal.enqueued(),
                Queued::Ignored | Queued::Dropped => {}
            }
            Ok(())
        })
    }
}

/// What the operation did to the one photo or video it concerns, and the key that identifies the
/// file across operations; `None` when Immich has nothing to learn from it.
fn change_of(op: &AppliedOp) -> Option<(i64, Effect)> {
    match op {
        AppliedOp::Moved { file_id, from, to } => {
            let effect = Effect {
                old: media_place(from),
                new: media_place(to),
            };
            (!effect.is_noop()).then_some((*file_id, effect))
        }
        AppliedOp::Copied { file_id, to, .. } => Some((*file_id, Effect::added(media_place(to)?))),
        AppliedOp::Trashed {
            file_ids,
            from,
            trash_item_id,
            is_dir: false,
            ..
        } => {
            // A row a scan already dropped is not listed: the trash item stands in for it. It is
            // an id of the trash table, so the negative of it never meets a row id.
            let key = file_ids.first().copied().unwrap_or_else(|| -*trash_item_id);
            Some((key, Effect::removed(media_place(from)?)))
        }
        AppliedOp::Restored {
            file_id: Some(file_id),
            to,
            is_dir: false,
            ..
        } => Some((*file_id, Effect::added(media_place(to)?))),
        AppliedOp::Trashed { .. } | AppliedOp::Restored { .. } | AppliedOp::Purged { .. } => None,
    }
}

/// The place of `loc` when it holds a photo or a video.
fn media_place(loc: &Loc) -> Option<Place> {
    let name = loc.name();
    // Photo and video extensions come first in the classification, so the sidecar list is moot.
    matches!(
        classify_name::<&str>(name, &[]),
        FileKind::Image | FileKind::Video
    )
    .then(|| Place::new(loc.volume_id, loc.dir().as_str(), name))
}

#[cfg(test)]
mod tests {
    use apm_core::paths::RelPath;

    use super::*;
    use crate::immich::outbox::test_support::Fixture;

    fn loc(volume_id: i64, rel: &str) -> Loc {
        Loc::new(volume_id, RelPath::new(rel).unwrap())
    }

    fn moved(file_id: i64, volume_id: i64, from: &str, to: &str) -> AppliedOp {
        AppliedOp::Moved {
            file_id,
            from: loc(volume_id, from),
            to: loc(volume_id, to),
        }
    }

    fn trashed(file_ids: &[i64], trash_item_id: i64, volume_id: i64, from: &str) -> AppliedOp {
        AppliedOp::Trashed {
            file_ids: file_ids.to_vec(),
            from: loc(volume_id, from),
            trash_item_id,
            trash_path: RelPath::new(".apm-trash/1/x").unwrap(),
            is_dir: false,
        }
    }

    #[tokio::test]
    async fn a_moved_photo_is_queued_with_both_places() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        fx.apply(&moved(7, volume, "Inbox/a.jpg", "2024/Trip/a.jpg"))
            .await;

        let rows = fx.rows().await;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(
            row.effect,
            Effect::moved(
                Place::new(volume, "Inbox", "a.jpg"),
                Place::new(volume, "2024/Trip", "a.jpg")
            )
        );
        assert_eq!((row.stage, row.attempts, row.carried), (0, 0, false));
        assert_eq!(row.scan_requested_at, None);
        assert!(fx.signal.last_enqueue().is_some());
    }

    #[tokio::test]
    async fn a_file_at_the_volume_root_has_an_empty_folder() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        fx.apply(&moved(7, volume, "a.jpg", "Trip/a.jpg")).await;
        let rows = fx.rows().await;
        assert_eq!(rows[0].effect.old, Some(Place::new(volume, "", "a.jpg")));
    }

    #[tokio::test]
    async fn nothing_is_queued_without_a_connection() {
        let fx = Fixture::new().await;
        let volume = fx.mapped_volume("photos").await;
        fx.apply(&moved(7, volume, "a.jpg", "b.jpg")).await;
        assert!(fx.rows().await.is_empty());
        assert_eq!(fx.signal.last_enqueue(), None);
    }

    #[tokio::test]
    async fn nothing_is_queued_for_an_unmapped_volume() {
        let fx = Fixture::connected().await;
        let mapped = fx.mapped_volume("photos").await;
        let other = fx.volume("scratch").await;
        fx.apply(&moved(7, other, "a.jpg", "b.jpg")).await;
        assert!(fx.rows().await.is_empty());

        // One mapped side is enough: the file left or entered what Immich watches.
        fx.apply(&AppliedOp::Moved {
            file_id: 8,
            from: loc(other, "a.jpg"),
            to: loc(mapped, "a.jpg"),
        })
        .await;
        assert_eq!(fx.rows().await.len(), 1);
    }

    #[tokio::test]
    async fn only_photos_and_videos_are_queued() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        for (id, name) in [
            (1, "a.xmp"),
            (2, "a.jpg.json"),
            (3, "song.flac"),
            (4, "notes.txt"),
            (5, "README"),
            (6, "clip.mov.aae"),
        ] {
            fx.apply(&moved(
                id,
                volume,
                &format!("x/{name}"),
                &format!("y/{name}"),
            ))
            .await;
        }
        assert!(fx.rows().await.is_empty());
        for (id, name) in [(11, "a.JPG"), (12, "b.cr3"), (13, "c.MOV"), (14, "d.heic")] {
            fx.apply(&moved(
                id,
                volume,
                &format!("x/{name}"),
                &format!("y/{name}"),
            ))
            .await;
        }
        assert_eq!(fx.rows().await.len(), 4);
    }

    #[tokio::test]
    async fn folders_and_purges_are_not_queued() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        fx.apply(&AppliedOp::Trashed {
            file_ids: vec![1, 2, 3],
            from: loc(volume, "Trip.jpg"),
            trash_item_id: 5,
            trash_path: RelPath::new(".apm-trash/5/Trip.jpg").unwrap(),
            is_dir: true,
        })
        .await;
        fx.apply(&AppliedOp::Restored {
            file_id: None,
            trash_item_id: 5,
            from: loc(volume, ".apm-trash/5/Trip.jpg"),
            to: loc(volume, "Trip.jpg"),
            is_dir: true,
        })
        .await;
        // An unindexed (junk) file restored: no row, nothing to queue.
        fx.apply(&AppliedOp::Restored {
            file_id: None,
            trash_item_id: 6,
            from: loc(volume, ".apm-trash/6/a.jpg"),
            to: loc(volume, "a.jpg"),
            is_dir: false,
        })
        .await;
        fx.apply(&AppliedOp::Purged {
            trash_item_id: 7,
            volume_id: volume,
            trash_path: RelPath::new(".apm-trash/7/a.jpg").unwrap(),
            is_dir: false,
            bytes: 10,
        })
        .await;
        assert!(fx.rows().await.is_empty());
    }

    #[tokio::test]
    async fn a_trashed_photo_is_a_removal_keyed_by_its_row() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        fx.apply(&trashed(&[9], 100, volume, "Trip/a.jpg")).await;
        let rows = fx.rows().await;
        assert_eq!(
            rows[0].effect,
            Effect::removed(Place::new(volume, "Trip", "a.jpg"))
        );
        assert_eq!(fx.file_ids().await, vec![9]);
    }

    #[tokio::test]
    async fn a_trash_without_a_listed_row_is_keyed_by_its_trash_item() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        fx.apply(&trashed(&[], 100, volume, "Trip/a.jpg")).await;
        fx.apply(&trashed(&[], 101, volume, "Trip/b.jpg")).await;
        assert_eq!(fx.file_ids().await, vec![-101, -100]);
        // Restoring creates a row of its own: it is a separate addition, not a merge.
        fx.apply(&AppliedOp::Restored {
            file_id: Some(55),
            trash_item_id: 100,
            from: loc(volume, ".apm-trash/100/a.jpg"),
            to: loc(volume, "Trip/a.jpg"),
            is_dir: false,
        })
        .await;
        assert_eq!(fx.rows().await.len(), 3);
    }

    #[tokio::test]
    async fn copies_and_restores_are_additions() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        fx.apply(&AppliedOp::Copied {
            source_file_id: Some(1),
            file_id: 20,
            from: loc(volume, "a/x.jpg"),
            to: loc(volume, "b/x.jpg"),
        })
        .await;
        fx.apply(&AppliedOp::Restored {
            file_id: Some(21),
            trash_item_id: 3,
            from: loc(volume, ".apm-trash/3/y.mp4"),
            to: loc(volume, "b/y.mp4"),
            is_dir: false,
        })
        .await;
        let rows = fx.rows().await;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.effect.old.is_none()));
        assert_eq!(rows[0].effect.new, Some(Place::new(volume, "b", "x.jpg")));
        assert_eq!(rows[1].effect.new, Some(Place::new(volume, "b", "y.mp4")));
    }

    #[tokio::test]
    async fn a_rename_that_changes_the_kind_counts_for_the_photo_side() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        fx.apply(&moved(1, volume, "a.jpg", "a.txt")).await;
        fx.apply(&moved(2, volume, "b.txt", "b.jpg")).await;
        let rows = fx.rows().await;
        assert_eq!(
            rows[0].effect,
            Effect::removed(Place::new(volume, "", "a.jpg"))
        );
        assert_eq!(
            rows[1].effect,
            Effect::added(Place::new(volume, "", "b.jpg"))
        );
    }

    #[tokio::test]
    async fn moves_of_one_file_merge_into_one_row() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        fx.apply(&moved(7, volume, "A/a.jpg", "B/a.jpg")).await;
        let first = fx.rows().await.remove(0);
        fx.apply(&moved(7, volume, "B/a.jpg", "C/a.jpg")).await;
        fx.apply(&moved(7, volume, "C/a.jpg", "C/b.jpg")).await;

        let rows = fx.rows().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, first.id);
        assert_eq!(
            rows[0].effect,
            Effect::moved(
                Place::new(volume, "A", "a.jpg"),
                Place::new(volume, "C", "b.jpg")
            )
        );
    }

    #[tokio::test]
    async fn a_move_and_its_undo_leave_no_row() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        fx.apply(&moved(7, volume, "A/a.jpg", "B/a.jpg")).await;
        fx.apply(&moved(7, volume, "B/a.jpg", "A/a.jpg")).await;
        assert!(fx.rows().await.is_empty());
    }

    #[tokio::test]
    async fn a_move_then_a_trash_is_one_removal_of_the_first_place() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        fx.apply(&moved(7, volume, "A/a.jpg", "B/a.jpg")).await;
        fx.apply(&trashed(&[7], 100, volume, "B/a.jpg")).await;
        let rows = fx.rows().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].effect,
            Effect::removed(Place::new(volume, "A", "a.jpg"))
        );
    }

    #[tokio::test]
    async fn a_change_after_the_scan_was_requested_is_a_row_of_its_own() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        fx.apply(&moved(7, volume, "A/a.jpg", "B/a.jpg")).await;
        fx.execute("UPDATE immich_outbox SET stage = 1").await;
        fx.apply(&moved(7, volume, "B/a.jpg", "C/a.jpg")).await;
        let rows = fx.rows().await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].stage, 1);
        assert_eq!(rows[1].stage, 0);
        assert_eq!(rows[1].effect.old, Some(Place::new(volume, "B", "a.jpg")));
    }

    #[tokio::test]
    async fn a_row_that_needs_attention_is_not_merged_into() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        fx.apply(&moved(7, volume, "A/a.jpg", "B/a.jpg")).await;
        fx.execute("UPDATE immich_outbox SET state = 'attention'")
            .await;
        fx.apply(&moved(7, volume, "B/a.jpg", "C/a.jpg")).await;
        assert_eq!(fx.rows().await.len(), 2);
    }

    #[tokio::test]
    async fn a_merge_makes_the_row_due_again_and_clears_its_error() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        fx.apply(&moved(7, volume, "A/a.jpg", "B/a.jpg")).await;
        fx.execute("UPDATE immich_outbox SET next_attempt_at = 4000000000, last_error = 'x'")
            .await;
        fx.apply(&moved(7, volume, "B/a.jpg", "C/a.jpg")).await;
        let (next, error): (i64, Option<String>) = fx
            .query("SELECT next_attempt_at, last_error FROM immich_outbox")
            .await;
        assert!(next < 4_000_000_000);
        assert_eq!(error, None);
    }

    #[tokio::test]
    async fn replaying_an_operation_leaves_one_identical_row() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        for op in [
            moved(7, volume, "A/a.jpg", "B/a.jpg"),
            trashed(&[8], 100, volume, "A/b.jpg"),
            AppliedOp::Copied {
                source_file_id: None,
                file_id: 9,
                from: loc(volume, "A/c.jpg"),
                to: loc(volume, "B/c.jpg"),
            },
        ] {
            fx.apply(&op).await;
            let once: Vec<_> = fx.rows().await.into_iter().map(|r| r.effect).collect();
            fx.apply(&op).await;
            let twice: Vec<_> = fx.rows().await.into_iter().map(|r| r.effect).collect();
            assert_eq!(once, twice);
        }
        assert_eq!(fx.rows().await.len(), 3);
    }

    #[tokio::test]
    async fn the_row_commits_and_rolls_back_with_the_operation() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        let hook = OutboxHook::new(fx.signal.clone());
        {
            let mut tx = fx.db.begin_write().await.unwrap();
            hook.on_applied(&mut tx, &moved(7, volume, "a.jpg", "b.jpg"))
                .await
                .unwrap();
            // The transaction is dropped without a commit: the operation failed.
        }
        assert!(fx.rows().await.is_empty());
        let mut tx = fx.db.begin_write().await.unwrap();
        hook.on_applied(&mut tx, &moved(7, volume, "a.jpg", "b.jpg"))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(fx.rows().await.len(), 1);
    }

    #[tokio::test]
    async fn only_a_failing_statement_is_an_error() {
        let fx = Fixture::connected().await;
        let volume = fx.mapped_volume("photos").await;
        let hook = OutboxHook::new(fx.signal.clone());
        fx.execute("DROP TABLE immich_outbox").await;
        let mut tx = fx.db.begin_write().await.unwrap();
        // Not a photo: no statement runs, so no error.
        hook.on_applied(&mut tx, &moved(7, volume, "a.txt", "b.txt"))
            .await
            .unwrap();
        // A photo: the statement fails and the operation has to roll back.
        assert!(
            hook.on_applied(&mut tx, &moved(7, volume, "a.jpg", "b.jpg"))
                .await
                .is_err()
        );
    }
}
