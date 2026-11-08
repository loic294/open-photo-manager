//! The accounting of an execution: ONE builder every executor uses, so the three planners do not
//! each write their own counters, failure list and summary.
//!
//! Every item the executor looks at is counted exactly once: [`ResultBuilder::done`],
//! [`ResultBuilder::skip`] or one of the `fail` methods. The first [`MAX_FAILURE_DETAILS`]
//! non-success items keep their details; `PlanResult::failed` and `skipped` count them separately.

use apm_core::dto::{ItemFailure, PlanResult};
use apm_core::paths::{FileId, RelPath, VolumeId};

use super::planner::ExecutePrep;
use crate::fsops::FsOpError;
use crate::indexer::VolumeRegistry;

/// How many non-success items a [`PlanResult`] keeps with their details.
pub const MAX_FAILURE_DETAILS: usize = 200;

/// Accumulates the outcome of the items of one execution.
#[derive(Debug, Default)]
pub struct ResultBuilder {
    result: PlanResult,
}

impl ResultBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// An item was processed; `bytes` is what it moved, copied or deleted (0 for a folder).
    pub fn done(&mut self, bytes: u64) {
        self.result.done += 1;
        self.result.bytes = self.result.bytes.saturating_add(bytes);
    }

    /// An item needed nothing: it was already in place, or its folder gained content meanwhile.
    pub fn skip(&mut self) {
        self.result.skipped += 1;
    }

    /// A safety refusal is a skip, with a stable code explaining why nothing was touched.
    pub fn skip_with_detail(
        &mut self,
        file_id: Option<FileId>,
        path: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.result.skipped += 1;
        self.detail(file_id, path, code, message);
    }

    /// An item could not be processed. `path` is for people (see [`display_path`]), `code` is
    /// stable (`FsOpError::code` or a code of the planner) and `message` never carries an
    /// absolute host path.
    pub fn fail(
        &mut self,
        file_id: Option<FileId>,
        path: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.result.failed += 1;
        self.detail(file_id, path, code, message);
    }

    fn detail(
        &mut self,
        file_id: Option<FileId>,
        path: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) {
        if self.result.failures.len() < MAX_FAILURE_DETAILS {
            self.result.failures.push(ItemFailure {
                file_id,
                path: path.into(),
                code: code.into(),
                message: message.into(),
            });
        }
    }

    /// An item failed in a file operation: its code and message come from the error.
    pub fn fail_fs(&mut self, file_id: Option<FileId>, path: impl Into<String>, err: &FsOpError) {
        // The cause of an internal error is for the log, not for the user.
        let message = match err {
            FsOpError::Internal(_) => "internal error".to_owned(),
            other => other.to_string(),
        };
        self.fail(file_id, path, err.code(), message);
    }

    /// The execution stopped before it went through every item.
    pub fn mark_cancelled(&mut self) {
        self.result.cancelled = true;
    }

    /// A sentence about how the execution ended that the counters do not say.
    pub fn note(&mut self, note: impl Into<String>) {
        self.result.note = Some(note.into());
    }

    /// Items counted so far: done, skipped and failed.
    pub fn counted(&self) -> u64 {
        self.result.done + self.result.skipped + self.result.failed
    }

    pub fn finish(self) -> PlanResult {
        self.result
    }
}

/// How people see an item: the volume name and the path below its root
/// ("Photos/2019/IMG_0001.jpg"), never an absolute path of the host. A volume the registry does
/// not know is "volume 7".
pub fn display_path(volumes: &VolumeRegistry, volume_id: VolumeId, rel: &RelPath) -> String {
    let volume = volumes
        .volume(volume_id)
        .map_or_else(|| format!("volume {volume_id}"), |volume| volume.name);
    if rel.is_root() {
        volume
    } else {
        format!("{volume}/{rel}")
    }
}

/// The title a finished task shows: "Deleted 2,988 of 3,000 files, 12 failed". `total` is the
/// number of items the executor declared, when it did.
pub fn summary_title(prep: &ExecutePrep, result: &PlanResult, total: Option<i64>) -> String {
    let total = total
        .and_then(|total| u64::try_from(total).ok())
        .filter(|total| *total >= result.done);
    let shown = total.unwrap_or(result.done);
    let noun = if shown == 1 {
        &prep.singular
    } else {
        &prep.plural
    };
    let mut title = match total {
        Some(total) if total != result.done => format!(
            "{} {} of {} {noun}",
            prep.verb,
            group_digits(result.done),
            group_digits(total)
        ),
        _ => format!("{} {} {noun}", prep.verb, group_digits(result.done)),
    };
    if result.skipped > 0 {
        title.push_str(&format!(", {} skipped", group_digits(result.skipped)));
    }
    if result.failed > 0 {
        title.push_str(&format!(", {} failed", group_digits(result.failed)));
    }
    if result.cancelled {
        title.push_str(" (cancelled)");
    }
    title
}

/// `2988` becomes "2,988".
fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prep() -> ExecutePrep {
        ExecutePrep::new("Delete files").summary("Deleted", "file", "files")
    }

    #[test]
    fn every_item_is_counted_once() {
        let mut builder = ResultBuilder::new();
        builder.done(100);
        builder.done(50);
        builder.skip();
        builder.fail(Some(7), "Photos/a.jpg", "source_changed", "it changed");
        assert_eq!(builder.counted(), 4);
        let result = builder.finish();
        assert_eq!(
            (result.done, result.skipped, result.failed, result.bytes),
            (2, 1, 1, 150)
        );
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].file_id, Some(7));
        assert!(!result.cancelled);
        assert_eq!(result.note, None);
    }

    #[test]
    fn coded_skips_and_failures_share_details_but_not_counters() {
        let mut builder = ResultBuilder::new();
        builder.done(4);
        builder.skip();
        builder.skip_with_detail(Some(1), "Photos/a.jpg", "no_survivor", "No survivor");
        builder.fail(Some(2), "Photos/b.jpg", "io_error", "Failure");
        assert_eq!(builder.counted(), 4);
        let result = builder.finish();
        assert_eq!((result.done, result.skipped, result.failed), (1, 2, 1));
        assert_eq!(result.failures.len(), 2);
        assert_eq!(
            summary_title(&prep(), &result, Some(4)),
            "Deleted 1 of 4 files, 2 skipped, 1 failed"
        );
        let mut builder = ResultBuilder::new();
        for id in 0..(MAX_FAILURE_DETAILS + 20) {
            if id % 2 == 0 {
                builder.skip_with_detail(Some(id as i64), "", "stale_match", "");
            } else {
                builder.fail(Some(id as i64), "", "io_error", "");
            }
        }
        let result = builder.finish();
        assert_eq!((result.skipped, result.failed), (110, 110));
        assert_eq!(result.failures.len(), MAX_FAILURE_DETAILS);
        assert_eq!(result.failures[199].file_id, Some(199));
    }

    #[test]
    fn only_the_first_failures_keep_their_details() {
        let mut builder = ResultBuilder::new();
        for index in 0..(MAX_FAILURE_DETAILS + 25) {
            builder.fail(
                Some(index as i64),
                format!("Photos/{index}.jpg"),
                "io_error",
                "boom",
            );
        }
        let result = builder.finish();
        assert_eq!(result.failed, (MAX_FAILURE_DETAILS + 25) as u64);
        assert_eq!(result.failures.len(), MAX_FAILURE_DETAILS);
        assert_eq!(result.failures[0].file_id, Some(0));
        assert_eq!(
            result.failures[MAX_FAILURE_DETAILS - 1].file_id,
            Some(MAX_FAILURE_DETAILS as i64 - 1)
        );
    }

    #[test]
    fn a_file_operation_error_brings_its_code_and_a_safe_message() {
        let mut builder = ResultBuilder::new();
        builder.fail_fs(Some(1), "Photos/a.jpg", &FsOpError::SourceChanged);
        builder.fail_fs(
            Some(2),
            "Photos/b.jpg",
            &FsOpError::Internal("/data/apm.db is locked".to_owned()),
        );
        let result = builder.finish();
        assert_eq!(result.failures[0].code, "source_changed");
        assert_eq!(
            result.failures[0].message,
            "the source file changed since the operation was planned"
        );
        assert_eq!(result.failures[1].code, "internal");
        assert_eq!(result.failures[1].message, "internal error");
    }

    #[test]
    fn cancellation_and_notes_are_kept() {
        let mut builder = ResultBuilder::new();
        builder.mark_cancelled();
        builder.note("stopped by the user");
        let result = builder.finish();
        assert!(result.cancelled);
        assert_eq!(result.note.as_deref(), Some("stopped by the user"));
    }

    #[test]
    fn digits_are_grouped_by_three() {
        let cases = [
            (0, "0"),
            (7, "7"),
            (999, "999"),
            (1000, "1,000"),
            (2988, "2,988"),
            (123_456, "123,456"),
            (1_234_567, "1,234,567"),
        ];
        for (value, expected) in cases {
            assert_eq!(group_digits(value), expected);
        }
    }

    #[test]
    fn the_summary_says_how_far_the_run_got() {
        let partial = PlanResult {
            done: 2988,
            failed: 12,
            ..PlanResult::default()
        };
        assert_eq!(
            summary_title(&prep(), &partial, Some(3000)),
            "Deleted 2,988 of 3,000 files, 12 failed"
        );

        let complete = PlanResult {
            done: 3000,
            ..PlanResult::default()
        };
        assert_eq!(
            summary_title(&prep(), &complete, Some(3000)),
            "Deleted 3,000 files"
        );
        assert_eq!(
            summary_title(&prep(), &complete, None),
            "Deleted 3,000 files"
        );

        let cancelled = PlanResult {
            done: 1200,
            skipped: 3,
            cancelled: true,
            ..PlanResult::default()
        };
        assert_eq!(
            summary_title(&prep(), &cancelled, Some(3000)),
            "Deleted 1,200 of 3,000 files, 3 skipped (cancelled)"
        );
    }

    #[test]
    fn the_summary_uses_the_singular_for_one() {
        let one = PlanResult {
            done: 1,
            ..PlanResult::default()
        };
        assert_eq!(summary_title(&prep(), &one, Some(1)), "Deleted 1 file");
        let none = PlanResult::default();
        assert_eq!(summary_title(&prep(), &none, Some(0)), "Deleted 0 files");
        assert_eq!(
            summary_title(&ExecutePrep::new("Run"), &one, None),
            "Processed 1 item"
        );
    }

    #[test]
    fn a_declared_total_below_the_done_count_is_ignored() {
        let result = PlanResult {
            done: 5,
            ..PlanResult::default()
        };
        assert_eq!(summary_title(&prep(), &result, Some(3)), "Deleted 5 files");
        assert_eq!(summary_title(&prep(), &result, Some(-1)), "Deleted 5 files");
    }

    #[tokio::test]
    async fn display_path_uses_the_volume_name_and_never_a_host_path() {
        let temp = crate::db::Db::open_temp().await.unwrap();
        let volumes = VolumeRegistry::load(temp.handle(), temp.dir().join("volumes"))
            .await
            .unwrap();
        let rel = RelPath::new("2019/IMG_0001.jpg").unwrap();
        assert_eq!(
            display_path(&volumes, 9, &rel),
            "volume 9/2019/IMG_0001.jpg"
        );
        assert_eq!(display_path(&volumes, 9, &RelPath::root()), "volume 9");
    }
}
