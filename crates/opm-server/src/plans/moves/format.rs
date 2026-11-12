use opm_core::dto::*;
use opm_core::pattern;

use super::build::source_statement;
use super::dest::{Source, destination_for};
use super::params;
use crate::db::models::capture_source_from_db;
use crate::plans::{PlanError, display_path};
use crate::state::AppState;

pub async fn preview(
    state: &AppState,
    request: FormatPreviewRequest,
) -> Result<FormatPreviewResponse, PlanError> {
    let query = params::source_query(&request.source)?;
    let limit = request.limit.unwrap_or(5);
    if limit == 0 || limit > 20 {
        return Err(PlanError::invalid_field(
            "limit",
            "limit must be between 1 and 20",
        ));
    }
    let parsed = match pattern::parse(&request.pattern) {
        Ok(p) => p,
        Err(e) => {
            return Ok(FormatPreviewResponse {
                error: Some(PatternIssue::from(e)),
                has_filename_token: false,
                appends_filename: false,
                samples: Vec::new(),
            });
        }
    };
    let p = MoveParams {
        move_type: MoveType::Custom,
        destination: FolderRef {
            volume_id: request
                .source
                .scope
                .volume_id
                .ok_or_else(|| PlanError::invalid_field("source.scope", "select a volume"))?,
            dir: "".into(),
        },
        source: request.source,
        pattern: Some(request.pattern),
        conflict_policy: None,
        match_mode: None,
        strict_prefix: false,
    };
    state.volumes.get(p.destination.volume_id)?;
    let rules = params::rules(&p, &state.settings.get())?;
    let mut statement = source_statement(&query);
    statement.push(" LIMIT ").push_bind(i64::from(limit));
    let rows: Vec<Source> = statement
        .build_query_as()
        .fetch_all(state.db.read())
        .await?;
    let mut samples = Vec::with_capacity(rows.len());
    for row in rows {
        let (destination, skip) = match destination_for(&rules, &row, None) {
            Ok(wanted) => (Some(wanted.loc()?.rel.as_str().to_owned()), None),
            Err(code) => (None, Some(code.into())),
        };
        let capture_src = row
            .capture_src
            .as_deref()
            .map(capture_source_from_db)
            .transpose()
            .map_err(anyhow::Error::from)?
            .or_else(|| {
                (row.capture_ts.is_none()
                    && row.meta_state != crate::db::models::PipelineState::Pending)
                    .then_some(CaptureSource::Fs)
            });
        samples.push(FormatSample {
            file_id: row.id,
            source: display_path(&state.volumes, row.volume_id, &row.loc()?.rel),
            destination,
            skip,
            capture_src,
        });
    }
    Ok(FormatPreviewResponse {
        error: None,
        has_filename_token: parsed.has_filename_token(),
        appends_filename: parsed.appends_filename(),
        samples,
    })
}

pub fn tokens() -> FormatTokensResponse {
    let file = pattern::sample_context();
    FormatTokensResponse {
        tokens: pattern::describe()
            .into_iter()
            .map(FormatToken::from)
            .collect(),
        examples: pattern::examples()
            .into_iter()
            .map(FormatExample::from)
            .collect(),
        sample: FormatSampleFile {
            file_name: file.file_name,
            captured: file.captured.and_utc().timestamp(),
            camera: file.camera,
            folder: file.folder,
        },
    }
}
