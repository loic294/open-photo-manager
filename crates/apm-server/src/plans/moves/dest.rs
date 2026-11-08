use apm_core::datetime::{CaptureInputs, resolve_capture};
use apm_core::dto::MoveType;
use apm_core::hash::Sha1Digest;
use apm_core::junk::JunkRules;
use apm_core::matching::MatchFacts;
use apm_core::paths::{FileId, RelPath, VolumeId};
use apm_core::pattern::{FileContext, Pattern, RenderOptions, TokenGroup};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use crate::db::models::PipelineState;
use crate::fsops::Loc;
use crate::plans::PlanError;

pub(super) const COLUMNS: &str = "f.id, f.volume_id, f.dir_path, f.name, f.size, f.mtime, f.btime, f.capture_ts, f.capture_src, f.meta_state, f.hash, cam.name AS camera";

#[derive(Clone, Debug, sqlx::FromRow)]
pub(super) struct Source {
    pub id: FileId,
    pub volume_id: VolumeId,
    pub dir_path: String,
    pub name: String,
    pub size: i64,
    pub mtime: i64,
    pub btime: Option<i64>,
    pub capture_ts: Option<i64>,
    pub capture_src: Option<String>,
    pub meta_state: PipelineState,
    pub hash: Option<Vec<u8>>,
    pub camera: Option<String>,
}

impl Source {
    pub fn loc(&self) -> Result<Loc, PlanError> {
        Ok(Loc::new(
            self.volume_id,
            RelPath::new(&self.dir_path)
                .and_then(|dir| dir.join(&self.name))
                .map_err(anyhow::Error::from)?,
        ))
    }
    pub fn digest(&self) -> Result<Option<Sha1Digest>, PlanError> {
        self.hash
            .as_deref()
            .map(|bytes| {
                bytes
                    .try_into()
                    .map_err(|_| PlanError::Internal(anyhow::anyhow!("invalid indexed SHA-1")))
            })
            .transpose()
    }
    pub fn facts(&self) -> Result<MatchFacts<'_>, PlanError> {
        Ok(MatchFacts {
            id: self.id,
            name: &self.name,
            hash: self.digest()?,
            capture_ts: self.capture_ts,
            size: self.size,
        })
    }
}

pub(super) struct Rules {
    pub move_type: MoveType,
    pub source_base: RelPath,
    pub destination_base: RelPath,
    pub destination_volume: VolumeId,
    pub pattern: Option<Pattern>,
    pub name_max_len: usize,
    pub junk: JunkRules,
    pub timezone: Tz,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Wanted {
    pub volume_id: VolumeId,
    pub dir: RelPath,
    pub name: String,
}

impl Wanted {
    pub fn loc(&self) -> Result<Loc, PlanError> {
        Ok(Loc::new(
            self.volume_id,
            self.dir.join(&self.name).map_err(anyhow::Error::from)?,
        ))
    }
}

impl Rules {
    pub fn uses_dates(&self) -> bool {
        self.pattern
            .as_ref()
            .is_some_and(|p| p.tokens().any(|t| t.group() == TokenGroup::Date))
    }
}

pub(super) fn structural_directory_for(
    rules: &Rules,
    current_dir: &RelPath,
    counterpart_dir: Option<&RelPath>,
) -> Result<RelPath, &'static str> {
    match rules.move_type {
        MoveType::KeepSource => Ok(rules.destination_base.join_rel(
            &current_dir
                .strip_prefix(&rules.source_base)
                .ok_or("pattern_failed")?,
        )),
        MoveType::Flat => Ok(rules.destination_base.clone()),
        MoveType::KeepDestination => Ok(counterpart_dir
            .cloned()
            .unwrap_or_else(|| rules.destination_base.clone())),
        MoveType::Custom => Err("pattern_failed"),
    }
}

pub(super) fn destination_for(
    rules: &Rules,
    row: &Source,
    counterpart_dir: Option<&RelPath>,
) -> Result<Wanted, &'static str> {
    if row.meta_state == PipelineState::Pending {
        return Err("metadata_pending");
    }
    let current_dir = RelPath::new(&row.dir_path).map_err(|_| "pattern_failed")?;
    let source_path = current_dir.join(&row.name).map_err(|_| "pattern_failed")?;
    if super::params::reserved(&rules.junk, &source_path) {
        return Err("reserved_destination");
    }
    let (dir, name) = match rules.move_type {
        MoveType::KeepSource | MoveType::Flat | MoveType::KeepDestination => (
            structural_directory_for(rules, &current_dir, counterpart_dir)?,
            row.name.clone(),
        ),
        MoveType::Custom => {
            let captured = match row.capture_ts {
                Some(ts) => DateTime::<Utc>::from_timestamp(ts, 0).map(|dt| dt.naive_utc()),
                None => resolve_capture(
                    &CaptureInputs {
                        birth_time: row.btime,
                        modified_time: row.mtime,
                        ..CaptureInputs::default()
                    },
                    &rules.timezone,
                )
                .naive(),
            }
            .ok_or("pattern_failed")?;
            let pattern = rules.pattern.as_ref().ok_or("pattern_failed")?;
            let rel = pattern.render_with(
                &FileContext {
                    file_name: row.name.clone(),
                    captured,
                    camera: row.camera.clone(),
                    folder: current_dir.file_name().unwrap_or("").to_owned(),
                },
                &RenderOptions {
                    max_component_bytes: rules.name_max_len,
                    ..RenderOptions::default()
                },
            );
            let name = rel.file_name().ok_or("pattern_failed")?.to_owned();
            (
                rules
                    .destination_base
                    .join_rel(&rel.parent().ok_or("pattern_failed")?),
                name,
            )
        }
    };
    let path = dir.join(&name).map_err(|_| "pattern_failed")?;
    if super::params::reserved(&rules.junk, &path) {
        return Err("reserved_destination");
    }
    Ok(Wanted {
        volume_id: rules.destination_volume,
        dir,
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use apm_core::pattern::parse;

    fn row() -> Source {
        Source {
            id: 1,
            volume_id: 1,
            dir_path: "base/Holiday".into(),
            name: "IMG.jpg".into(),
            size: 1,
            mtime: 0,
            btime: None,
            capture_ts: Some(1_791_205_200),
            capture_src: Some("xmp".into()),
            meta_state: PipelineState::Done,
            hash: None,
            camera: Some("Canon/Camera".into()),
        }
    }
    fn rules(move_type: MoveType) -> Rules {
        Rules {
            move_type,
            source_base: RelPath::new("base").unwrap(),
            destination_base: RelPath::new("out").unwrap(),
            destination_volume: 2,
            pattern: Some(parse("{YYYY}/{mm}").unwrap()),
            name_max_len: 143,
            junk: JunkRules::reserved_only(),
            timezone: chrono_tz::UTC,
        }
    }
    #[test]
    fn all_destinations_and_metadata_gate() {
        let mut row = row();
        for (kind, expected) in [
            (MoveType::KeepSource, "out/Holiday/IMG.jpg"),
            (MoveType::Flat, "out/IMG.jpg"),
            (MoveType::KeepDestination, "out/IMG.jpg"),
            (MoveType::Custom, "out/2026/10/IMG.jpg"),
        ] {
            let r = rules(kind);
            assert_eq!(
                destination_for(&r, &row, None)
                    .unwrap()
                    .loc()
                    .unwrap()
                    .rel
                    .as_str(),
                expected
            );
            row.meta_state = PipelineState::Pending;
            assert_eq!(destination_for(&r, &row, None), Err("metadata_pending"));
            row.meta_state = PipelineState::Done;
        }
        assert_eq!(
            destination_for(
                &rules(MoveType::KeepDestination),
                &row,
                Some(&RelPath::new("out/there").unwrap())
            )
            .unwrap()
            .dir
            .as_str(),
            "out/there"
        );
    }
    #[test]
    fn birth_time_folder_names_and_reserved_paths() {
        let mut row = row();
        row.capture_ts = None;
        row.btime = Some(1_791_205_200);
        let mut r = rules(MoveType::Custom);
        r.pattern = Some(parse("{YYYY}/{mm}/{folder}/{name}_{camera}.{ext}").unwrap());
        let dst = destination_for(&r, &row, None).unwrap();
        assert_eq!(dst.dir.as_str(), "out/2026/10/Holiday");
        assert_eq!(dst.name, "IMG_Canon_Camera.jpg");
        r.pattern = Some(parse(".apm-trash/{filename}").unwrap());
        assert_eq!(destination_for(&r, &row, None), Err("reserved_destination"));
    }

    #[test]
    fn indexed_capture_precedes_birth_time_and_modified_time_is_last_fallback() {
        let mut row = row();
        row.btime = Some(1);
        row.mtime = 1_704_067_200;
        let r = rules(MoveType::Custom);
        assert_eq!(
            destination_for(&r, &row, None).unwrap().dir.as_str(),
            "out/2026/10"
        );
        row.capture_ts = None;
        assert_eq!(
            destination_for(&r, &row, None).unwrap().dir.as_str(),
            "out/1970/01"
        );
        row.btime = None;
        assert_eq!(
            destination_for(&r, &row, None).unwrap().dir.as_str(),
            "out/2024/01"
        );
        row.btime = Some(0);
        assert_eq!(
            destination_for(&r, &row, None).unwrap().dir.as_str(),
            "out/2024/01"
        );
    }

    proptest::proptest! {
        #[test]
        fn custom_values_do_not_escape_and_name_caps_hold(
            camera in "[a-zA-Z/:?]{0,300}",
            folder in "[a-zA-Z]{1,40}",
            stem in "[a-zA-Z]{1,300}",
        ) {
            let mut row = row();
            row.camera = Some(camera);
            row.dir_path = format!("base/{folder}");
            row.name = format!("{stem}.jpg");
            let mut r = rules(MoveType::Custom);
            r.pattern = Some(parse("{camera}/{folder}/{name}.{ext}").unwrap());
            let got = destination_for(&r, &row, None).unwrap();
            proptest::prop_assert!(got.dir.starts_with(&RelPath::new("out").unwrap()));
            proptest::prop_assert!(got.name.len() <= 143);
            proptest::prop_assert!(got.name.ends_with(".jpg"));
            for component in got.dir.components() {
                proptest::prop_assert!(component.len() <= 143);
                proptest::prop_assert!(!matches!(component, "." | ".."));
            }
        }
    }
}
