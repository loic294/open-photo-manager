use opm_core::dto::{MoveParams, MoveType, SideParams};
use opm_core::junk::JunkRules;
use opm_core::paths::RelPath;
use opm_core::pattern::{Pattern, parse};

use super::dest::Rules;
use crate::indexer::VolumeRegistry;
use crate::plans::PlanError;
use crate::query::{FileQuery, KindFilter, QueryScope};
use crate::settings::Settings;

pub(crate) fn source_query(source: &SideParams) -> Result<FileQuery, PlanError> {
    if source.scope.volume_id.is_none() {
        return Err(PlanError::invalid_field(
            "source.scope",
            "select a volume as the source base",
        ));
    }
    let scope = QueryScope::try_from(&source.scope)
        .map_err(|e| PlanError::invalid_field("source.scope", e.to_string()))?;
    Ok(FileQuery::new(scope, &source.filter)
        .map_err(|e| PlanError::invalid_field("source.filter", e.to_string()))?
        .kinds(KindFilter::All))
}

pub(super) fn pattern(text: &str) -> Result<Pattern, PlanError> {
    parse(text).map_err(|e| {
        PlanError::invalid_field(
            "pattern",
            format!(
                "{} at {}..{}: {}; suggestion: {}",
                e.code(),
                e.span.start,
                e.span.end,
                e,
                e.suggestion().as_deref().unwrap_or("none")
            ),
        )
    })
}

pub(super) fn rules(p: &MoveParams, settings: &Settings) -> Result<Rules, PlanError> {
    source_query(&p.source)?;
    let source_base = RelPath::new(&p.source.scope.dir)
        .map_err(|e| PlanError::invalid_field("source.scope", e.to_string()))?;
    let destination_base = RelPath::new(&p.destination.dir)
        .map_err(|e| PlanError::invalid_field("destination", e.to_string()))?;
    let parsed = match (p.move_type, p.pattern.as_deref()) {
        (MoveType::Custom, Some(text)) => Some(pattern(text)?),
        (MoveType::Custom, None) => {
            return Err(PlanError::invalid_field(
                "pattern",
                "custom structure requires a pattern",
            ));
        }
        (_, Some(_)) => {
            return Err(PlanError::invalid_field(
                "pattern",
                "only custom structure accepts a pattern",
            ));
        }
        (_, None) => None,
    };
    if p.move_type != MoveType::KeepDestination && (p.match_mode.is_some() || p.strict_prefix) {
        return Err(PlanError::invalid_field(
            "match_mode",
            "matching options are only used for keep_destination",
        ));
    }
    if p.strict_prefix && p.match_mode != Some(opm_core::dto::MatchMode::NamePrefix) {
        return Err(PlanError::invalid_field(
            "match_mode",
            "strict_prefix requires name_prefix",
        ));
    }
    let junk = JunkRules::new(&settings.junk_names, &settings.junk_prefixes);
    for (field, path) in [
        ("source.scope", &source_base),
        ("destination", &destination_base),
    ] {
        if reserved(&junk, path) {
            return Err(PlanError::invalid_field(
                field,
                "the base is a reserved or ignored folder",
            ));
        }
    }
    Ok(Rules {
        move_type: p.move_type,
        source_base,
        destination_base,
        destination_volume: p.destination.volume_id,
        pattern: parsed,
        name_max_len: usize::from(settings.name_max_len),
        junk,
        timezone: opm_core::datetime::tz_or_utc(std::env::var("TZ").ok().as_deref()),
    })
}

pub(crate) fn reserved(junk: &JunkRules, path: &RelPath) -> bool {
    crate::fsops::is_protected_path(path) || junk.is_junk_path(path)
}

pub(super) fn volumes(p: &MoveParams, registry: &VolumeRegistry) -> Result<(), PlanError> {
    for (field, id) in [
        ("source.scope", p.source.scope.volume_id),
        ("destination", Some(p.destination.volume_id)),
    ] {
        super::validate_volume(field, id, registry)?;
    }
    Ok(())
}

pub(crate) fn validate_volume(
    field: &str,
    id: Option<opm_core::paths::VolumeId>,
    registry: &VolumeRegistry,
) -> Result<(), PlanError> {
    let volume = id
        .and_then(|id| registry.volume(id))
        .ok_or_else(|| PlanError::invalid_field(field, "unknown volume"))?;
    if !volume.online || !volume.writable {
        return Err(PlanError::invalid_field(
            field,
            "the volume must be online and writable",
        ));
    }
    Ok(())
}
