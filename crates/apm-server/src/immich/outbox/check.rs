//! Phase 3 of a pass: for a row whose scan had time to settle, read the filesystem and Immich
//! afresh and settle the row. Carries what the user set over to the new asset, then deletes the old
//! one, but only when every proof of the safety rules holds (`decide`).
//!
//! Everything this phase believes was read in the same step: the old path on the volume, the old
//! asset (`GET /assets/{id}`, never the stored copy of Immich's list) and the new asset. After a
//! carry-over made calls, all of it is read once more right before the delete.

use super::carry::{self, CarryError, Snapshot, is_gone};
use super::decide::{Disk, Facts, NewSide, OldAsset, Verdict, decide};
use super::merge::Place;
use super::probe::{place_path, probe};
use super::step::{
    Finish, Shared, Step, attention_limit, code, failed, retry_later, storage_failed, wait_a_while,
};
use super::store::{self, Patch, Row};
use crate::immich::assets;
use crate::immich::models::{ImmichAsset, OfflineAssetId};

/// Settles one row that was prepared and scanned in an earlier pass or earlier in this one.
pub(super) async fn check(sh: Shared<'_>, row: &Row) -> Step {
    match run(sh, row).await {
        Ok(step) => step,
        Err(step) => *step,
    }
}

async fn run(sh: Shared<'_>, row: &Row) -> Result<Step, Box<Step>> {
    let (Some(old), Some(asset_id)) = (row.effect.old.as_ref(), row.asset_id.as_deref()) else {
        return Ok(Step::finished(Finish::Dropped));
    };
    let Some(old_path) = place_path(sh.map, old) else {
        return Ok(Step::finished(Finish::Dropped));
    };
    let mut seen = look(sh, row, old, asset_id, &old_path).await?;
    let mut carried = row.carried;
    let mut carried_now = false;
    let mut verdict = decide(&seen.facts(carried, row.offline_seen_at, sh.now));
    if verdict == (Verdict::Delete { carry: true }) {
        match seen.new_asset.clone() {
            Some(new_asset) => {
                carried_now = carry_over(sh, row, &new_asset).await?;
                if carried_now {
                    remember_carried(sh, row, &new_asset).await;
                    seen = look(sh, row, old, asset_id, &old_path).await?;
                }
                carried = true;
                verdict = decide(&seen.facts(carried, row.offline_seen_at, sh.now));
            }
            // A ready new asset is always there; if it is not, nothing is deleted.
            None => {
                verdict = Verdict::Wait {
                    first_offline: false,
                };
            }
        }
    }
    let learned = Patch {
        carried: (carried && !row.carried).then_some(true),
        new_asset_id: seen
            .new_asset
            .as_ref()
            .map(|asset| asset.id.clone())
            .filter(|id| row.new_asset_id.as_deref() != Some(id.as_str())),
        ..Patch::default()
    };
    Ok(match verdict {
        Verdict::Superseded => Step::finished(Finish::Superseded),
        Verdict::Gone => {
            forget(sh, asset_id).await;
            Step::finished(Finish::Gone)
        }
        Verdict::PathMismatch => Step::attention(row, code::PATH_MISMATCH),
        Verdict::Unreadable => retry_later(row, sh.now, code::UNREADABLE, None, learned),
        Verdict::Wait { first_offline } => {
            let waiting_for = match seen.old {
                OldAsset::Found { offline: false, .. } => code::NOT_OFFLINE,
                _ => code::SCAN_TIMEOUT,
            };
            let extra = Patch {
                offline_seen_at: first_offline.then_some(sh.now),
                ..learned
            };
            wait_a_while(row, sh.now, attention_limit(row), waiting_for, extra)
        }
        Verdict::Delete { .. } => {
            if delete_offline(sh, row, seen.old_asset.as_ref()).await? {
                forget(sh, asset_id).await;
                Step::finished(Finish::Deleted {
                    carried: carried_now || row.carried,
                })
            } else {
                Step::finished(Finish::Superseded)
            }
        }
    })
}

// ----- what was seen ----------------------------------------------------------------------------

/// What the volume and Immich showed for one row, read in one go.
struct Seen {
    disk: Disk,
    old: OldAsset,
    /// The fresh read of the old asset, when Immich has one at the old path.
    old_asset: Option<ImmichAsset>,
    new: NewSide,
    /// The fresh read of the asset that replaces the old one, when there is one.
    new_asset: Option<ImmichAsset>,
}

impl Seen {
    /// A look that did not go further than the volume.
    fn blind(disk: Disk) -> Self {
        Self {
            disk,
            old: OldAsset::Missing,
            old_asset: None,
            new: NewSide::Missing,
            new_asset: None,
        }
    }

    fn facts(&self, carried: bool, offline_seen_at: Option<i64>, now: i64) -> Facts {
        Facts {
            disk: self.disk,
            old: self.old,
            new: self.new,
            carried,
            offline_seen_at,
            now,
        }
    }
}

/// Reads the old path on the volume and, only when it is empty, the old asset and the new side.
/// Nothing of Immich is asked while the file may still be there.
async fn look(
    sh: Shared<'_>,
    row: &Row,
    old: &Place,
    asset_id: &str,
    old_path: &str,
) -> Result<Seen, Box<Step>> {
    let disk = probe(sh.volumes, old).await;
    if disk != Disk::Absent {
        return Ok(Seen::blind(disk));
    }
    let fresh = sh
        .client
        .get_asset(asset_id)
        .await
        .map_err(|err| failed(row, sh.now, &err, code::IMMICH_ERROR))?;
    let Some(asset) = fresh else {
        return Ok(Seen::blind(disk));
    };
    let path_matches = asset.original_path == old_path;
    if !path_matches && followed_the_file(sh, row, &asset) {
        return Ok(Seen {
            old: OldAsset::Moved,
            ..Seen::blind(disk)
        });
    }
    let old = OldAsset::Found {
        offline: asset.is_offline,
        path_matches,
    };
    let (new, new_asset) = if asset.is_offline && path_matches {
        look_at_new(sh, row).await?
    } else {
        (NewSide::Missing, None)
    };
    Ok(Seen {
        disk,
        old,
        old_asset: Some(asset),
        new,
        new_asset,
    })
}

/// Whether Immich moved `asset` along with the file: it is live at the NEW place of the row.
fn followed_the_file(sh: Shared<'_>, row: &Row, asset: &ImmichAsset) -> bool {
    asset.is_live()
        && row
            .effect
            .new
            .as_ref()
            .and_then(|place| place_path(sh.map, place))
            .is_some_and(|path| path == asset.original_path)
}

/// What Immich has for the new place of the file: the asset recorded in the row, then the live one
/// the stored list shows there, each read afresh and accepted only at the right `originalPath`.
async fn look_at_new(
    sh: Shared<'_>,
    row: &Row,
) -> Result<(NewSide, Option<ImmichAsset>), Box<Step>> {
    let Some(new) = &row.effect.new else {
        return Ok((NewSide::Empty, None));
    };
    let Some(new_path) = place_path(sh.map, new) else {
        return Ok((NewSide::Unmapped, None));
    };
    let mut candidates: Vec<String> = row.new_asset_id.iter().cloned().collect();
    let known = assets::find_by_location(sh.db, new.volume_id, &new.dir, &new.name)
        .await
        .map_err(|err| storage_failed(&err))?;
    if let Some(live) = known.iter().find(|record| record.is_live())
        && !candidates.contains(&live.asset_id)
    {
        candidates.push(live.asset_id.clone());
    }
    for id in candidates {
        match sh.client.get_asset(&id).await {
            Ok(Some(asset)) if asset.is_live() && asset.original_path == new_path => {
                let side = if asset.has_metadata {
                    NewSide::Ready
                } else {
                    NewSide::Importing
                };
                return Ok((side, Some(asset)));
            }
            Ok(_) => {}
            Err(err) => return Err(failed(row, sh.now, &err, code::IMMICH_ERROR)),
        }
    }
    // No asset at the new place. A file that is there is waiting to be imported or lies in a folder
    // Immich skips; one that is not (it moved again, or was trashed) leaves nothing to conclude
    // from, so the row keeps waiting.
    Ok(match probe(sh.volumes, new).await {
        Disk::Present => (NewSide::Missing, None),
        Disk::Absent | Disk::Unknown => (NewSide::Importing, None),
    })
}

// ----- carrying and deleting --------------------------------------------------------------------

/// Carries the snapshot of the row onto the fresh `new_asset`. `Ok(true)`: Immich was called.
async fn carry_over(sh: Shared<'_>, row: &Row, new_asset: &ImmichAsset) -> Result<bool, Box<Step>> {
    let Some(snapshot) = row.snapshot_json.as_deref().and_then(Snapshot::from_json) else {
        return Ok(false);
    };
    let plan = snapshot.plan(new_asset);
    if plan.is_empty() {
        return Ok(false);
    }
    match carry::apply(sh.client, &new_asset.id, &plan).await {
        Ok(()) => Ok(true),
        Err(CarryError::Refused(why)) => {
            tracing::warn!(row = row.id, %why, "Immich refused to take over what the user set");
            Err(Box::new(Step::attention(row, code::CARRY_FAILED)))
        }
        Err(CarryError::Immich(err)) => Err(failed(row, sh.now, &err, code::CARRY_FAILED)),
    }
}

/// Writes down, at once, that the carry-over was made: a pass that is cancelled or crashes before
/// the delete then goes straight to the delete. Not fatal when it fails (the carry-over is
/// idempotent), so it only warns.
async fn remember_carried(sh: Shared<'_>, row: &Row, new_asset: &ImmichAsset) {
    let patch = Patch {
        carried: Some(true),
        new_asset_id: Some(new_asset.id.clone()),
        ..Patch::default()
    };
    if let Err(err) = store::apply(sh.db, &[(row.clone(), patch)], &[]).await {
        tracing::warn!(row = row.id, error = %err, "the outbox could not record a carry-over");
    }
}

/// Deletes the old asset of a row, which `decide` showed offline. The id must come from the fresh
/// read of the same step (`OfflineAssetId::confirm`): `Ok(false)` when it is not offline in it.
/// `force` is safe because Immich only removes the record of an offline asset, never a file.
async fn delete_offline(
    sh: Shared<'_>,
    row: &Row,
    old_asset: Option<&ImmichAsset>,
) -> Result<bool, Box<Step>> {
    let Some(old_asset) = old_asset else {
        return Ok(false);
    };
    let Some(id) = OfflineAssetId::confirm(old_asset) else {
        return Ok(false);
    };
    match sh.client.delete_assets(&[id], true).await {
        Ok(()) => Ok(true),
        Err(err) if is_gone(&err) => match sh.client.get_asset(&old_asset.id).await {
            Ok(None) => Ok(true),
            Ok(Some(_)) => Err(failed(row, sh.now, &err, code::IMMICH_ERROR)),
            Err(other) => Err(failed(row, sh.now, &other, code::IMMICH_ERROR)),
        },
        Err(err) => Err(failed(row, sh.now, &err, code::IMMICH_ERROR)),
    }
}

/// Drops the stored copy of an asset that left Immich, so the list shows what Immich has.
async fn forget(sh: Shared<'_>, asset_id: &str) {
    if let Err(err) = assets::forget(sh.db, &[asset_id.to_owned()]).await {
        tracing::warn!(error = %err, "the stored list of Immich assets was not updated");
    }
}
