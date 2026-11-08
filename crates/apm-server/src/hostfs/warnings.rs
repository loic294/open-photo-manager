//! Stable host warnings shared by health and index-status responses.

use apm_core::dto::HostWarning;
use apm_core::dto::SettingsDto;

use crate::indexer::VolumeRegistry;

#[derive(Debug, Default)]
pub struct WarningInputs {
    pub polling_volumes: Vec<String>,
    pub unreliable_inode_volumes: Vec<String>,
    pub rescan_interval_minutes: u32,
    pub watch_enabled: bool,
}

pub fn for_state(volumes: &VolumeRegistry, settings: &SettingsDto) -> Vec<HostWarning> {
    let polling_volumes = if settings.index.watch_enabled {
        volumes
            .online()
            .into_iter()
            .filter_map(|volume| {
                let (mode, _) = volumes.watch_mode(volume.id, true);
                (mode == apm_core::dto::WatchMode::PollingOnly).then_some(volume.name)
            })
            .collect()
    } else {
        Vec::new()
    };
    let unreliable_inode_volumes = volumes
        .online()
        .into_iter()
        .filter_map(|volume| {
            volumes
                .capabilities(volume.id)
                .is_some_and(|capabilities| capabilities.inode_reliable == Some(false))
                .then_some(volume.name)
        })
        .collect();
    build(WarningInputs {
        polling_volumes,
        unreliable_inode_volumes,
        rescan_interval_minutes: settings.index.rescan_interval_minutes,
        watch_enabled: settings.index.watch_enabled,
    })
}

pub fn build(mut input: WarningInputs) -> Vec<HostWarning> {
    let mut warnings = Vec::new();
    if input.watch_enabled {
        input.polling_volumes.sort();
        input.polling_volumes.dedup();
        if !input.polling_volumes.is_empty() {
            let interval = input.rescan_interval_minutes;
            let cadence = if interval == 0 {
                "automatic rescans are off".to_owned()
            } else {
                format!("the full rescan interval is {interval} minutes")
            };
            warnings.push(HostWarning {
                code: "volumes_polling_only".to_owned(),
                message: format!(
                    "Changes made outside the app on {} are noticed only by the next rescan; {cadence}. Use Re-index or choose a shorter interval in Settings.",
                    input.polling_volumes.join(", ")
                ),
            });
        }
    }
    input.unreliable_inode_volumes.sort();
    input.unreliable_inode_volumes.dedup();
    if !input.unreliable_inode_volumes.is_empty() {
        warnings.push(HostWarning {
            code: "unreliable_volume_inodes".to_owned(),
            message: format!(
                "The filesystem reports unreliable inode identity for {}. File operations may fail safely; move the library to a filesystem with stable inode numbers.",
                input.unreliable_inode_volumes.join(", ")
            ),
        });
    }
    warnings.sort_by(|left, right| left.code.cmp(&right.code));
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_builder_is_deterministic_and_obeys_disabled_watch_and_zero_interval() {
        let warnings = build(WarningInputs {
            polling_volumes: vec!["zeta".into(), "photos".into(), "photos".into()],
            unreliable_inode_volumes: vec!["zeta".into()],
            rescan_interval_minutes: 0,
            watch_enabled: true,
        });
        assert_eq!(
            warnings
                .iter()
                .map(|warning| warning.code.as_str())
                .collect::<Vec<_>>(),
            ["unreliable_volume_inodes", "volumes_polling_only"]
        );
        assert!(warnings[1].message.contains("photos, zeta"));
        assert!(warnings[1].message.contains("automatic rescans are off"));
        assert!(warnings[1].message.contains("Re-index"));

        let disabled = build(WarningInputs {
            polling_volumes: vec!["photos".into()],
            watch_enabled: false,
            ..WarningInputs::default()
        });
        assert!(disabled.is_empty());
    }
}
