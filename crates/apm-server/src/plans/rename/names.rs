use apm_core::datetime::{CaptureInputs, resolve_capture};
use apm_core::dto::RenameIssueField;
use apm_core::junk::JunkRules;
use apm_core::paths::RelPath;
use apm_core::pattern::{TokenGroup, parse_name};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use super::super::moves::Source;
use crate::db::models::PipelineState;
use crate::plans::PlanError;
use crate::settings::Settings;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Unmatched,
    Pending,
    Failed(&'static str),
    Rename { name: String, seq: Option<u64> },
}

pub(crate) struct Names {
    compiled: apm_core::rename::CompiledRules,
    junk: JunkRules,
    timezone: Tz,
    name_max_len: usize,
    uses_dates: bool,
}

impl Names {
    pub(crate) fn new(
        compiled: apm_core::rename::CompiledRules,
        pattern: &str,
        settings: &Settings,
    ) -> Result<Self, PlanError> {
        let parsed = parse_name(pattern).map_err(|error| {
            PlanError::Internal(anyhow::anyhow!(
                "compiled rename pattern no longer parses: {error}"
            ))
        })?;
        Ok(Self {
            compiled,
            junk: JunkRules::new(&settings.junk_names, &settings.junk_prefixes),
            timezone: apm_core::datetime::tz_or_utc(std::env::var("TZ").ok().as_deref()),
            name_max_len: usize::from(settings.name_max_len),
            uses_dates: parsed
                .tokens()
                .any(|token| token.group() == TokenGroup::Date),
        })
    }

    pub(crate) fn uses_dates(&self) -> bool {
        self.uses_dates
    }

    pub(crate) fn uses_metadata(&self) -> bool {
        self.compiled.needs_metadata()
    }

    pub(crate) fn uses_count(&self) -> bool {
        self.compiled.uses_count()
    }

    pub(crate) fn matches(&self, name: &str) -> bool {
        self.compiled.matches(name)
    }

    pub(crate) fn folder(&self, rows: &[Source]) -> Result<Vec<Outcome>, PlanError> {
        let mut outcomes = vec![Outcome::Unmatched; rows.len()];
        let mut renderable = Vec::new();

        for (index, row) in rows.iter().enumerate() {
            if !self.compiled.matches(&row.name) {
                continue;
            }
            if row.meta_state == PipelineState::Pending && self.uses_metadata() {
                outcomes[index] = Outcome::Pending;
                continue;
            }
            if self.source_reserved(row) {
                outcomes[index] = Outcome::Failed("reserved_destination");
                continue;
            }
            let file = match self.file(row) {
                Ok(file) => file,
                Err(code) => {
                    outcomes[index] = Outcome::Failed(code);
                    continue;
                }
            };
            match apm_core::rename::new_name(&self.compiled, &file, Some(1)) {
                Ok(Some(name)) => renderable.push((index, file, name)),
                Ok(None) => {}
                Err(_) => outcomes[index] = Outcome::Failed("pattern_failed"),
            }
        }

        let files: Vec<_> = renderable.iter().map(|(_, file, _)| *file).collect();
        let counts = apm_core::rename::assign_counts(&self.compiled, &files).map_err(|error| {
            PlanError::Internal(anyhow::anyhow!(
                "valid rename candidates could not be counted: {error}"
            ))
        })?;
        for (index, file, first_name) in renderable {
            let seq = self
                .uses_count()
                .then(|| counts.get(&file.id).copied().unwrap_or(1));
            let name = if let Some(count) = seq {
                match apm_core::rename::new_name(&self.compiled, &file, Some(count)) {
                    Ok(Some(name)) => name,
                    Ok(None) => continue,
                    Err(_) => {
                        outcomes[index] = Outcome::Failed("pattern_failed");
                        continue;
                    }
                }
            } else {
                first_name
            };
            outcomes[index] = if self.destination_reserved(&rows[index], &name) {
                Outcome::Failed("reserved_destination")
            } else {
                Outcome::Rename { name, seq }
            };
        }
        Ok(outcomes)
    }

    pub(crate) fn one(&self, row: &Source, seq: Option<u64>) -> Outcome {
        if !self.compiled.matches(&row.name) {
            return Outcome::Unmatched;
        }
        if row.meta_state == PipelineState::Pending && self.uses_metadata() {
            return Outcome::Pending;
        }
        if self.source_reserved(row) {
            return Outcome::Failed("reserved_destination");
        }
        let file = match self.file(row) {
            Ok(file) => file,
            Err(code) => return Outcome::Failed(code),
        };
        match apm_core::rename::new_name(&self.compiled, &file, seq) {
            Ok(Some(name)) if self.destination_reserved(row, &name) => {
                Outcome::Failed("reserved_destination")
            }
            Ok(Some(name)) => Outcome::Rename { name, seq },
            Ok(None) => Outcome::Unmatched,
            Err(_) => Outcome::Failed("pattern_failed"),
        }
    }

    fn file<'a>(&self, row: &'a Source) -> Result<apm_core::rename::RenameFile<'a>, &'static str> {
        let captured = match row.capture_ts {
            Some(timestamp) => DateTime::<Utc>::from_timestamp(timestamp, 0)
                .map(|date| date.naive_utc())
                .ok_or("pattern_failed")?,
            None => resolve_capture(
                &CaptureInputs {
                    birth_time: row.btime,
                    modified_time: row.mtime,
                    ..CaptureInputs::default()
                },
                &self.timezone,
            )
            .naive()
            .ok_or("pattern_failed")?,
        };
        RelPath::new(&row.dir_path).map_err(|_| "pattern_failed")?;
        Ok(apm_core::rename::RenameFile {
            id: row.id,
            name: &row.name,
            captured,
            capture_ts: row.capture_ts,
            camera: row.camera.as_deref(),
            folder: row
                .dir_path
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(""),
            max_component_bytes: self.name_max_len,
        })
    }

    fn destination_reserved(&self, row: &Source, name: &str) -> bool {
        let Ok(dir) = RelPath::new(&row.dir_path) else {
            return true;
        };
        let Ok(path) = dir.join(name) else {
            return true;
        };
        super::super::moves::reserved(&self.junk, &path)
    }

    fn source_reserved(&self, row: &Source) -> bool {
        let Ok(dir) = RelPath::new(&row.dir_path) else {
            return true;
        };
        let Ok(path) = dir.join(&row.name) else {
            return true;
        };
        super::super::moves::reserved(&self.junk, &path)
    }
}

pub(crate) fn issue_field(field: RenameIssueField) -> &'static str {
    match field {
        RenameIssueField::Matcher => "matcher",
        RenameIssueField::Rewrite => "rewrite",
        RenameIssueField::Pattern => "pattern",
        RenameIssueField::CountDigits => "count_digits",
    }
}

#[cfg(test)]
mod tests {
    use apm_core::dto::{RenameMatchMode, RenameMatcher, RenameRewrite, RenameRules};

    use super::*;

    fn source(id: i64, name: &str, dir_path: &str, meta_state: PipelineState) -> Source {
        Source {
            id,
            volume_id: 1,
            dir_path: dir_path.to_owned(),
            name: name.to_owned(),
            size: 1,
            mtime: 1_700_000_000,
            btime: None,
            capture_ts: Some(1_700_000_000),
            capture_src: Some("exif".to_owned()),
            meta_state,
            hash: None,
            camera: Some("Camera".to_owned()),
        }
    }

    fn names(rules: RenameRules) -> Names {
        let compiled = apm_core::rename::compile(&rules).expect("valid rules");
        Names::new(compiled, &rules.pattern, &Settings::defaults_for(1, 1)).expect("valid pattern")
    }

    fn rules(pattern: &str) -> RenameRules {
        RenameRules {
            matcher: RenameMatcher {
                mode: RenameMatchMode::All,
                pattern: String::new(),
                ignore_case: true,
            },
            clean: false,
            rewrite: None,
            pattern: pattern.to_owned(),
            count_digits: 0,
        }
    }

    #[test]
    fn folder_counters_restart_and_preserve_extensions() {
        let mut rules = rules("Photo_{count}");
        rules.count_digits = 3;
        let names = names(rules);
        for dir in ["one", "two"] {
            let rows = [
                source(2, "b.JPG", dir, PipelineState::Done),
                source(1, "a.JPG", dir, PipelineState::Done),
            ];
            let outcomes = names.folder(&rows).expect("names");
            assert_eq!(
                outcomes,
                [
                    Outcome::Rename {
                        name: "Photo_002.JPG".to_owned(),
                        seq: Some(2),
                    },
                    Outcome::Rename {
                        name: "Photo_001.JPG".to_owned(),
                        seq: Some(1),
                    },
                ]
            );
        }
    }

    #[test]
    fn one_failed_empty_stem_does_not_consume_a_folder_counter() {
        let mut rules = rules("{name}_{count}");
        rules.rewrite = Some(RenameRewrite {
            find: "^bad$".to_owned(),
            replace: String::new(),
            ignore_case: false,
        });
        let names = names(rules);
        let rows = [
            source(1, "bad.jpg", "photos", PipelineState::Done),
            source(2, "good.jpg", "photos", PipelineState::Done),
        ];
        assert_eq!(
            names.folder(&rows).expect("names"),
            [
                Outcome::Failed("pattern_failed"),
                Outcome::Rename {
                    name: "good_1.jpg".to_owned(),
                    seq: Some(1),
                },
            ]
        );
    }

    #[test]
    fn pending_metadata_is_only_a_skip_when_rules_need_it() {
        let pending = source(1, "photo.jpg", "photos", PipelineState::Pending);
        assert_eq!(
            names(rules("New_{name}")).one(&pending, None),
            Outcome::Rename {
                name: "New_photo.jpg".to_owned(),
                seq: None,
            }
        );
        assert_eq!(
            names(rules("{camera}_{name}")).one(&pending, None),
            Outcome::Pending
        );
    }

    #[test]
    fn matching_and_case_only_names_use_the_original_bytes() {
        let mut rules = rules("a");
        rules.matcher = RenameMatcher {
            mode: RenameMatchMode::Glob,
            pattern: "*.jpg".to_owned(),
            ignore_case: true,
        };
        let names = names(rules);
        assert_eq!(
            names.one(&source(1, "A.jpg", "photos", PipelineState::Done), None),
            Outcome::Rename {
                name: "a.jpg".to_owned(),
                seq: None,
            }
        );
        assert_eq!(
            names.one(&source(2, "notes.txt", "photos", PipelineState::Done), None),
            Outcome::Unmatched
        );
    }

    #[test]
    fn clean_and_rewrite_share_the_core_name_pipeline() {
        let mut rules = rules("{name}");
        rules.clean = true;
        rules.rewrite = Some(RenameRewrite {
            find: "^IMG_".to_owned(),
            replace: "Photo_".to_owned(),
            ignore_case: false,
        });
        assert_eq!(
            names(rules).one(
                &source(1, "IMG_001 (1).jpg", "photos", PipelineState::Done),
                None
            ),
            Outcome::Rename {
                name: "Photo_001.jpg".to_owned(),
                seq: None,
            }
        );
    }

    #[test]
    fn reserved_source_paths_are_refused() {
        assert_eq!(
            names(rules("New_{name}")).one(
                &source(1, "photo.jpg", "photos/#recycle", PipelineState::Done),
                None
            ),
            Outcome::Failed("reserved_destination")
        );
    }
}
