use futures_util::TryStreamExt;
use opm_core::dto::{
    RenamePreviewRequest, RenamePreviewResponse, RenameSample, RenameSampleOutcome,
};
use opm_core::paths::VolumeId;

use super::super::moves::{Source, source_query, source_statement, validate_volume};
use super::names::{Names, Outcome};
use crate::plans::PlanError;
use crate::state::AppState;

const MAX_PREVIEW_MATCHES: usize = 20_000;

pub(crate) async fn preview(
    state: &AppState,
    request: RenamePreviewRequest,
) -> Result<RenamePreviewResponse, PlanError> {
    let compiled = match opm_core::rename::compile(&request.rules) {
        Ok(compiled) => compiled,
        Err(issues) => {
            return Ok(RenamePreviewResponse {
                issues,
                volume_id: request.source.scope.volume_id,
                dir: request.source.scope.dir,
                ..RenamePreviewResponse::default()
            });
        }
    };
    let query = source_query(&request.source)?;
    validate_volume(
        "source.scope.volume_id",
        request.source.scope.volume_id,
        &state.volumes,
    )?;
    let names = Names::new(compiled, &request.rules.pattern, &state.settings.get())?;
    let limit = request.limit.unwrap_or(30).clamp(1, 200) as usize;
    let mut read = state.db.read().begin().await?;
    let mut statement = source_statement(&query);
    let mut rows = statement.build_query_as::<Source>().fetch(&mut *read);
    let mut folder: Option<(VolumeId, String)> = None;
    let mut folder_files = 0_u64;
    let mut matched = 0_u64;
    let mut matching_rows = Vec::new();
    let mut sample_volume = request.source.scope.volume_id;
    let mut sample_dir = request.source.scope.dir.clone();
    let mut found = false;

    while let Some(row) = rows.try_next().await? {
        let key = (row.volume_id, row.dir_path.clone());
        if folder.as_ref().is_some_and(|current| current != &key) {
            if matched > 0 {
                found = true;
                break;
            }
            folder_files = 0;
            matching_rows.clear();
            matched = 0;
        }
        folder = Some(key);
        folder_files += 1;
        if names.matches(&row.name) {
            matched += 1;
            if matching_rows.len() < MAX_PREVIEW_MATCHES {
                matching_rows.push(row);
            }
        }
    }
    if !found && matched > 0 {
        found = true;
    }
    drop(rows);
    read.commit().await?;

    if let Some((volume, dir)) = folder.filter(|_| found) {
        sample_volume = Some(volume);
        sample_dir = dir;
    } else {
        folder_files = 0;
        matched = 0;
        matching_rows.clear();
    }
    let truncated = matched > matching_rows.len() as u64;
    let outcomes = names.folder(&matching_rows)?;
    let samples = matching_rows
        .iter()
        .zip(outcomes)
        .filter_map(|(row, outcome)| match outcome {
            Outcome::Unmatched => None,
            Outcome::Pending => Some(RenameSample {
                file_id: row.id,
                name: row.name.clone(),
                new_name: None,
                outcome: RenameSampleOutcome::Pending,
                error: None,
            }),
            Outcome::Failed(code) => Some(RenameSample {
                file_id: row.id,
                name: row.name.clone(),
                new_name: None,
                outcome: RenameSampleOutcome::Failed,
                error: Some(code.to_owned()),
            }),
            Outcome::Rename { name, .. } => Some(RenameSample {
                file_id: row.id,
                name: row.name.clone(),
                outcome: if name == row.name {
                    RenameSampleOutcome::Unchanged
                } else {
                    RenameSampleOutcome::Rename
                },
                new_name: Some(name),
                error: None,
            }),
        })
        .take(limit)
        .collect();
    Ok(RenamePreviewResponse {
        issues: Vec::new(),
        volume_id: sample_volume,
        dir: sample_dir,
        folder_files,
        matched,
        truncated,
        samples,
    })
}
