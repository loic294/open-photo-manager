//! The libraries of Immich, the volume-to-library mappings and the exclusion of the trash folder.
//!
//! # Mappings
//!
//! A mapping pairs a volume with the path prefix Immich uses for its root and, in full mode, with
//! the Immich library that holds it. [`PathMap`](super::PathMap) is built from the stored set and is
//! the one translation between Immich paths and files.
//!
//! `PUT /api/immich/mappings` carries the complete set and replaces the stored one in one
//! transaction, so saving the same set twice changes nothing. A set is checked completely before
//! anything is stored, and every problem is reported at once in one validation error whose fields
//! name the entry (`mappings[1].path_prefix`): the rules of [`paths::validate`], that every volume
//! exists and, in full mode, that every library exists in Immich. What needs no answer from Immich
//! is checked first; a set that breaks such a rule is refused without contacting Immich.
//!
//! In limited mode (an account that is not an administrator) Immich lists no libraries, so a
//! library cannot be chosen or checked: the mapping is only the path prefix, which is all the sync
//! and the outbox need.
//!
//! # The trash folder
//!
//! Deleting in the application moves a file to `<volume>/.apm-trash/...`. The library scans of
//! Immich skip hidden directories, but its file watcher may still notice the moved file and import
//! it again. [`ImmichService::exclude_trash`] adds the exclusion pattern `**/.apm-trash/**` to
//! every library a mapping uses. The list of exclusion patterns is replaced by `PUT /libraries/{id}`,
//! so the complete list is read first and sent back with the pattern appended; nothing the
//! administrator configured is lost.

use std::collections::HashSet;

use apm_core::consts::TRASH_DIR;
use apm_core::dto::{
    FieldError, ImmichExcludeTrashResult, ImmichLibrariesResponse, ImmichLibraryDto,
    ImmichLibraryFailure, ImmichLibraryRef, ImmichMappingEntry, ImmichMappingsResponse,
};

use crate::db::volumes;

use super::ImmichService;
use super::client::ImmichClient;
use super::error::ImmichError;
use super::models::ImmichLibrary;
use super::paths;
use super::store::{self, MappingInput};

impl ImmichService {
    /// The external libraries of Immich, live. In limited mode (no administrator) Immich does not
    /// list libraries: the answer is then an empty list marked `limited`, without asking Immich.
    ///
    /// The remembered library names of the mappings are refreshed from what Immich says.
    pub async fn libraries(&self) -> Result<ImmichLibrariesResponse, ImmichError> {
        let stored = self.stored().await?;
        if !stored.is_admin {
            return Ok(ImmichLibrariesResponse {
                libraries: Vec::new(),
                limited: true,
            });
        }
        let client = self.client_for(&stored).await?;
        let libraries = client.libraries().await?;
        self.refresh_library_names(&libraries).await;
        Ok(ImmichLibrariesResponse {
            libraries: libraries.into_iter().map(library_dto).collect(),
            limited: false,
        })
    }

    /// Replaces the stored mappings by `entries`; see the module documentation. Every problem of
    /// the set comes back together as [`ImmichError::Validation`].
    pub async fn set_mappings(
        &self,
        entries: Vec<ImmichMappingEntry>,
    ) -> Result<ImmichMappingsResponse, ImmichError> {
        let stored = self.stored().await?;
        let mut problems = paths::validate(&entries, stored.is_admin);
        problems.extend(self.unknown_volumes(&entries).await?);
        if !problems.is_empty() {
            return Err(ImmichError::Validation(problems));
        }

        let live = if stored.is_admin && !entries.is_empty() {
            Some(self.client_for(&stored).await?.libraries().await?)
        } else {
            None
        };
        if let Some(live) = &live {
            let unknown = unknown_libraries(&entries, live);
            if !unknown.is_empty() {
                return Err(ImmichError::Validation(unknown));
            }
        }

        let inputs: Vec<MappingInput> = entries
            .into_iter()
            .map(|entry| MappingInput {
                library_name: name_of(live.as_deref(), entry.library_id.as_deref()),
                volume_id: entry.volume_id,
                library_id: entry.library_id,
                path_prefix: entry.path_prefix,
            })
            .collect();
        store::replace_mappings(&self.inner.db, &inputs)
            .await
            .map_err(|err| {
                // A volume that vanished between the check and the save.
                if err.is_foreign_key_violation()
                    || err.is_unique_violation()
                    || err.is_check_violation()
                {
                    ImmichError::invalid_field(
                        "mappings",
                        "the volumes changed while saving; reload the page and try again",
                    )
                } else {
                    ImmichError::from(err)
                }
            })?;
        Ok(ImmichMappingsResponse {
            mappings: store::mappings(&self.inner.db).await?,
        })
    }

    /// Adds the exclusion pattern of the trash folder to every library the mappings use, once.
    ///
    /// Administrators only ([`ImmichError::AdminRequired`] otherwise). A library that cannot be
    /// updated is reported in `failed` and does not stop the others, except when Immich cannot be
    /// reached or refuses the key: that would fail every library, so the whole call fails.
    pub async fn exclude_trash(&self) -> Result<ImmichExcludeTrashResult, ImmichError> {
        let stored = self.stored().await?;
        if !stored.is_admin {
            return Err(ImmichError::AdminRequired);
        }
        let client = self.client_for(&stored).await?;
        let pattern = trash_pattern();
        let mut result = ImmichExcludeTrashResult {
            pattern: pattern.clone(),
            updated: Vec::new(),
            already_excluded: Vec::new(),
            failed: Vec::new(),
        };

        let mut seen = HashSet::new();
        for mapping in store::mappings(&self.inner.db).await? {
            let Some(id) = mapping.library_id else {
                continue;
            };
            if !seen.insert(id.clone()) {
                continue;
            }
            match add_exclusion(&client, &id, &pattern).await {
                Ok(Excluded::Added(library)) => result.updated.push(library_ref(library)),
                Ok(Excluded::Present(library)) => {
                    result.already_excluded.push(library_ref(library));
                }
                Err(err) if err.affects_every_call() => return Err(err),
                Err(err) => result.failed.push(ImmichLibraryFailure {
                    id,
                    name: mapping.library_name,
                    message: err.summary(),
                }),
            }
        }
        Ok(result)
    }

    /// One problem per entry whose volume does not exist.
    async fn unknown_volumes(
        &self,
        entries: &[ImmichMappingEntry],
    ) -> Result<Vec<FieldError>, ImmichError> {
        let mut problems = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            if volumes::get(&self.inner.db, entry.volume_id)
                .await?
                .is_none()
            {
                problems.push(FieldError::new(
                    format!("mappings[{index}].volume_id"),
                    "is not a volume of this application",
                ));
            }
        }
        Ok(problems)
    }

    /// Stores the names Immich gives to the libraries the mappings use, so the settings page shows
    /// the current name. A failure only costs that: the listing itself is still right.
    async fn refresh_library_names(&self, live: &[ImmichLibrary]) {
        let mapped = match store::mappings(&self.inner.db).await {
            Ok(mapped) => mapped,
            Err(err) => {
                tracing::warn!(error = %err, "cannot read the Immich mappings to refresh library names");
                return;
            }
        };
        for mapping in mapped {
            let Some(id) = mapping.library_id.as_deref() else {
                continue;
            };
            let Some(library) = live.iter().find(|library| library.id == id) else {
                continue;
            };
            if library.name == mapping.library_name {
                continue;
            }
            if let Err(err) = store::record_library_name(&self.inner.db, id, &library.name).await {
                tracing::warn!(error = %err, "cannot store the name of an Immich library");
            }
        }
    }
}

/// The exclusion pattern that keeps the trash folder of any volume out of a library.
fn trash_pattern() -> String {
    format!("**/{TRASH_DIR}/**")
}

enum Excluded {
    Added(ImmichLibrary),
    Present(ImmichLibrary),
}

/// Makes sure `pattern` is among the exclusion patterns of library `id`.
async fn add_exclusion(
    client: &ImmichClient,
    id: &str,
    pattern: &str,
) -> Result<Excluded, ImmichError> {
    let library = client.library(id).await?;
    if library
        .exclusion_patterns
        .iter()
        .any(|existing| existing == pattern)
    {
        return Ok(Excluded::Present(library));
    }
    let mut patterns = library.exclusion_patterns.clone();
    patterns.push(pattern.to_owned());
    client.update_library_exclusions(id, &patterns).await?;
    Ok(Excluded::Added(library))
}

/// One problem per entry whose library Immich does not have.
fn unknown_libraries(entries: &[ImmichMappingEntry], live: &[ImmichLibrary]) -> Vec<FieldError> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let id = entry.library_id.as_deref()?;
            (!live.iter().any(|library| library.id == id)).then(|| {
                FieldError::new(
                    format!("mappings[{index}].library_id"),
                    "is not a library of this Immich server",
                )
            })
        })
        .collect()
}

/// The name Immich gives to library `id`; empty when it is not known.
fn name_of(live: Option<&[ImmichLibrary]>, id: Option<&str>) -> String {
    let (Some(live), Some(id)) = (live, id) else {
        return String::new();
    };
    live.iter()
        .find(|library| library.id == id)
        .map(|library| library.name.clone())
        .unwrap_or_default()
}

fn library_dto(library: ImmichLibrary) -> ImmichLibraryDto {
    ImmichLibraryDto {
        id: library.id,
        name: library.name,
        import_paths: library.import_paths,
        exclusion_patterns: library.exclusion_patterns,
        asset_count: library.asset_count,
    }
}

fn library_ref(library: ImmichLibrary) -> ImmichLibraryRef {
    ImmichLibraryRef {
        id: library.id,
        name: library.name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(id: &str, name: &str) -> ImmichLibrary {
        ImmichLibrary {
            id: id.to_owned(),
            name: name.to_owned(),
            import_paths: vec!["/mnt/photo".to_owned()],
            exclusion_patterns: vec!["**/@eaDir/**".to_owned()],
            asset_count: 12,
        }
    }

    fn entry(volume_id: i64, library_id: Option<&str>, prefix: &str) -> ImmichMappingEntry {
        ImmichMappingEntry {
            volume_id,
            library_id: library_id.map(str::to_owned),
            path_prefix: prefix.to_owned(),
        }
    }

    #[test]
    fn the_trash_pattern_follows_the_trash_folder_constant() {
        assert_eq!(trash_pattern(), "**/.apm-trash/**");
        assert!(trash_pattern().contains(TRASH_DIR));
    }

    #[test]
    fn unknown_libraries_are_named_by_the_entry() {
        let live = [library("lib-1", "Photos")];
        let entries = [
            entry(1, Some("lib-1"), "/mnt/a"),
            entry(2, Some("lib-gone"), "/mnt/b"),
            entry(3, None, "/mnt/c"),
        ];
        let problems = unknown_libraries(&entries, &live);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].field, "mappings[1].library_id");
    }

    #[test]
    fn the_library_name_comes_from_the_live_list_or_stays_empty() {
        let live = [library("lib-1", "Photos")];
        assert_eq!(name_of(Some(&live), Some("lib-1")), "Photos");
        assert_eq!(name_of(Some(&live), Some("other")), "");
        assert_eq!(name_of(Some(&live), None), "");
        assert_eq!(name_of(None, Some("lib-1")), "");
    }

    #[test]
    fn a_library_keeps_every_field_in_the_view() {
        let dto = library_dto(library("lib-1", "Photos"));
        assert_eq!(dto.id, "lib-1");
        assert_eq!(dto.name, "Photos");
        assert_eq!(dto.import_paths, ["/mnt/photo"]);
        assert_eq!(dto.exclusion_patterns, ["**/@eaDir/**"]);
        assert_eq!(dto.asset_count, 12);
    }
}
