//! How a restore or purge run reports what happened: the counters, the failures it kept and the
//! two outputs a task has (the JSON result of a clean run, the error text of a run with failures).

use apm_core::paths::TrashItemId;
use serde_json::{Value, json};

use crate::tasks::TaskError;

/// Failures kept in the result of a run. A run that fails on thousands of items still has a result
/// of a few kilobytes.
const KEPT_FAILURES: usize = 200;
/// Failures the error text of a failed task names; the rest is counted.
const NAMED_FAILURES: usize = 3;

/// One item that could not be restored or deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Failure {
    pub id: TrashItemId,
    /// Where the item was before it was trashed, relative to the volume root.
    pub path: String,
    /// The `FsOpError::code` of the failure, or `not_in_trash` for an id that has no row.
    pub code: &'static str,
    pub message: String,
}

/// The tally of one run.
#[derive(Debug, Default)]
pub(super) struct Report {
    /// Items brought back or deleted.
    pub done: u64,
    /// Items left in the trash on purpose (a name that was taken) or already gone (a row that
    /// vanished while the run was queued).
    pub skipped: u64,
    /// Bytes of the items that are `done`.
    pub bytes: u64,
    failed: u64,
    failures: Vec<Failure>,
    /// What the title says about the skipped items ("left in the trash"); nothing when empty.
    skipped_note: &'static str,
}

impl Report {
    pub fn new(skipped_note: &'static str) -> Self {
        Self {
            skipped_note,
            ..Self::default()
        }
    }

    pub fn fail(&mut self, failure: Failure) {
        self.failed += 1;
        if self.failures.len() < KEPT_FAILURES {
            self.failures.push(failure);
        }
    }

    pub fn failed(&self) -> u64 {
        self.failed
    }

    /// The title a finished task keeps: "Restored 118 of 120 items, 2 left in the trash" or
    /// "Restored 120 items".
    pub fn title(&self, verb: &str, total: u64) -> String {
        let items = if total == 1 { "item" } else { "items" };
        let mut title = if self.done == total {
            format!("{verb} {total} {items}")
        } else {
            format!("{verb} {} of {total} {items}", self.done)
        };
        if self.skipped > 0 && !self.skipped_note.is_empty() {
            title.push_str(&format!(", {} {}", self.skipped, self.skipped_note));
        }
        title
    }

    /// The task's outcome. A run without failures is `done` with a JSON summary (`done_key` names
    /// the counter of the items that were restored or purged). A run with failures is `failed` with
    /// a text that counts them and names the first ones: the task indicator keeps a failed task
    /// until the user dismisses it, so a partial failure is not missed.
    pub fn outcome(&self, done_key: &str, total: u64, verb: &str) -> Result<Value, TaskError> {
        if self.failed == 0 {
            return Ok(json!({
                done_key: self.done,
                "skipped": self.skipped,
                "failed": 0,
                "bytes": self.bytes,
                "failures": [],
            }));
        }
        Err(TaskError::failed(self.error_text(total, verb)))
    }

    /// The JSON summary of the run, failures included (for the log and for tests).
    pub fn summary(&self, done_key: &str) -> Value {
        json!({
            done_key: self.done,
            "skipped": self.skipped,
            "failed": self.failed,
            "bytes": self.bytes,
            "failures": self.failures.iter().map(|failure| json!({
                "id": failure.id,
                "path": failure.path,
                "code": failure.code,
                "message": failure.message,
            })).collect::<Vec<_>>(),
        })
    }

    fn error_text(&self, total: u64, verb: &str) -> String {
        let named = self
            .failures
            .iter()
            .take(NAMED_FAILURES)
            .map(|failure| {
                format!(
                    "{} ({}: {})",
                    display_path(&failure.path),
                    failure.code,
                    failure.message
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let more = self.failed.saturating_sub(NAMED_FAILURES as u64);
        let rest = if more > 0 {
            format!(", and {more} more")
        } else {
            String::new()
        };
        format!(
            "{} ({} failed): {named}{rest}",
            self.title(verb, total),
            self.failed
        )
    }
}

/// A path as the error text shows it: the root has no name.
fn display_path(path: &str) -> &str {
    if path.is_empty() {
        "(volume root)"
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(id: i64) -> Failure {
        Failure {
            id,
            path: format!("a/{id}.jpg"),
            code: "source_missing",
            message: "the source file does not exist".to_owned(),
        }
    }

    #[test]
    fn a_clean_run_is_a_json_summary() {
        let report = Report {
            done: 3,
            skipped: 1,
            bytes: 30,
            ..Report::default()
        };
        let outcome = report.outcome("restored", 4, "Restored").unwrap();
        assert_eq!(outcome["restored"], 3);
        assert_eq!(outcome["skipped"], 1);
        assert_eq!(outcome["failed"], 0);
        assert_eq!(outcome["bytes"], 30);
    }

    #[test]
    fn a_run_with_failures_names_the_first_three_and_counts_the_rest() {
        let mut report = Report {
            done: 2,
            ..Report::default()
        };
        for id in 1..=5 {
            report.fail(failure(id));
        }
        let Err(TaskError::Failed(text)) = report.outcome("restored", 7, "Restored") else {
            panic!("a failure must fail the task");
        };
        assert!(
            text.starts_with("Restored 2 of 7 items (5 failed): "),
            "{text}"
        );
        assert!(text.contains("a/1.jpg (source_missing: the source file does not exist)"));
        assert!(text.contains("a/3.jpg"));
        assert!(!text.contains("a/4.jpg"));
        assert!(text.ends_with(", and 2 more"), "{text}");
    }

    #[test]
    fn only_a_bounded_number_of_failures_is_kept() {
        let mut report = Report::default();
        for id in 0..1_000 {
            report.fail(failure(id));
        }
        assert_eq!(report.failed(), 1_000);
        assert_eq!(
            report.summary("purged")["failures"]
                .as_array()
                .unwrap()
                .len(),
            KEPT_FAILURES
        );
        assert_eq!(report.summary("purged")["failed"], 1_000);
    }

    #[test]
    fn the_title_pluralises() {
        let one = Report {
            done: 1,
            ..Report::default()
        };
        assert_eq!(one.title("Restored", 1), "Restored 1 item");
        assert_eq!(one.title("Restored", 2), "Restored 1 of 2 items");
        assert_eq!(Report::default().title("Deleted", 0), "Deleted 0 items");
    }

    #[test]
    fn the_title_counts_the_items_left_in_the_trash() {
        let mut report = Report::new("left in the trash");
        report.done = 2;
        report.skipped = 3;
        assert_eq!(
            report.title("Restored", 5),
            "Restored 2 of 5 items, 3 left in the trash"
        );
        report.skipped = 0;
        assert_eq!(report.title("Restored", 2), "Restored 2 items");
    }
}
