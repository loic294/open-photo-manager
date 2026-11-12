//! What to do with a row whose library scan was requested: a pure decision over the facts the
//! pass observed. The IO around it (`pass`) only gathers the facts and carries the verdict out, so
//! every safety rule that is about WHEN an old asset may be deleted is a line of this file and a
//! row of its tests.
//!
//! The rules, in order (the numbers are those of the safety rules in the module docs of `outbox`):
//! 1. S3: a file at the old path again, or an old path that cannot be read, never ends in a delete;
//! 2. an old asset Immich no longer knows, or one that followed the file by itself, needs nothing;
//! 3. S3: an asset whose path is not the old path is not ours to delete;
//! 4. only an asset that is OFFLINE (S1) is ever removed, and one that came back online is left alone;
//! 5. before removing it, the favorites, ratings, albums and tags are carried to the new asset when
//!    there is one, and when the new asset is still being imported we wait instead of deleting.

use super::consts::NEW_ASSET_GRACE_SECS;

/// What the filesystem says about the OLD path of the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disk {
    /// The volume proved that nothing is there.
    Absent,
    /// A file (or anything else) is there again.
    Present,
    /// The volume could not tell (offline, unreadable, a link in the way): not proof of anything.
    Unknown,
}

/// What a fresh `GET /assets/{id}` said about the old asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OldAsset {
    /// Immich does not know the asset (any more).
    Missing,
    /// The asset is live and its `originalPath` is the NEW place of the row: Immich followed the
    /// file by itself, so there is no old asset left to clean up.
    Moved,
    Found {
        offline: bool,
        /// Its `originalPath` is the old path of the row, mapped through the current `PathMap`.
        path_matches: bool,
    },
}

/// What is known about the asset Immich made for the NEW place of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewSide {
    /// The change has no new place (a trash): there will be no new asset.
    Empty,
    /// The new place is outside every mapped volume: Immich will never import it.
    Unmapped,
    /// The new place is in a library but Immich shows no asset for it (yet).
    Missing,
    /// An asset exists but Immich has not finished reading its metadata.
    Importing,
    /// An asset exists and its metadata is read.
    Ready,
}

/// Everything the decision depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    pub disk: Disk,
    pub old: OldAsset,
    pub new: NewSide,
    /// The favorites, ratings, albums and tags are already on the new asset.
    pub carried: bool,
    /// When a pass first saw the old asset offline.
    pub offline_seen_at: Option<i64>,
    pub now: i64,
}

/// The verdict for one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing is left to do and nothing was changed in Immich: the old asset is already gone.
    Gone,
    /// The file is back, the asset is online again or Immich followed the file by itself: the
    /// change no longer applies.
    Superseded,
    /// The old asset is not the file this row is about: a human decides.
    PathMismatch,
    /// The filesystem could not prove the old path is empty: try again later, delete nothing.
    Unreadable,
    /// Not ready: try again later. `first_offline` says this pass is the first one to see the old
    /// asset offline, so `offline_seen_at` must be recorded.
    Wait { first_offline: bool },
    /// Remove the old asset, after carrying its state over when `carry` is set.
    Delete { carry: bool },
}

/// See the module docs.
pub fn decide(facts: &Facts) -> Verdict {
    match facts.disk {
        Disk::Present => return Verdict::Superseded,
        Disk::Unknown => return Verdict::Unreadable,
        Disk::Absent => {}
    }
    let offline = match facts.old {
        OldAsset::Missing => return Verdict::Gone,
        OldAsset::Moved => return Verdict::Superseded,
        OldAsset::Found {
            path_matches: false,
            ..
        } => return Verdict::PathMismatch,
        OldAsset::Found { offline, .. } => offline,
    };
    if !offline {
        return if facts.offline_seen_at.is_some() {
            Verdict::Superseded
        } else {
            Verdict::Wait {
                first_offline: false,
            }
        };
    }
    let first_offline = facts.offline_seen_at.is_none();
    match facts.new {
        NewSide::Empty | NewSide::Unmapped => Verdict::Delete { carry: false },
        NewSide::Ready => Verdict::Delete {
            carry: !facts.carried,
        },
        NewSide::Importing => Verdict::Wait { first_offline },
        NewSide::Missing => match facts.offline_seen_at {
            Some(seen) if facts.now.saturating_sub(seen) >= NEW_ASSET_GRACE_SECS => {
                Verdict::Delete { carry: false }
            }
            _ => Verdict::Wait { first_offline },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 5_000_000;

    fn facts() -> Facts {
        Facts {
            disk: Disk::Absent,
            old: OldAsset::Found {
                offline: true,
                path_matches: true,
            },
            new: NewSide::Ready,
            carried: false,
            offline_seen_at: Some(NOW - 30),
            now: NOW,
        }
    }

    fn online() -> OldAsset {
        OldAsset::Found {
            offline: false,
            path_matches: true,
        }
    }

    #[test]
    fn an_offline_asset_with_a_ready_new_asset_is_carried_then_deleted() {
        assert_eq!(decide(&facts()), Verdict::Delete { carry: true });
    }

    #[test]
    fn a_carried_asset_is_deleted_without_carrying_again() {
        let done = Facts {
            carried: true,
            ..facts()
        };
        assert_eq!(decide(&done), Verdict::Delete { carry: false });
    }

    #[test]
    fn nothing_that_will_never_be_imported_waits_for_a_new_asset() {
        for new in [NewSide::Empty, NewSide::Unmapped] {
            let first = Facts {
                new,
                offline_seen_at: None,
                ..facts()
            };
            assert_eq!(decide(&first), Verdict::Delete { carry: false }, "{new:?}");
        }
    }

    #[test]
    fn a_file_that_is_back_at_the_old_path_is_superseded_whatever_immich_says() {
        for old in [
            OldAsset::Missing,
            OldAsset::Moved,
            online(),
            OldAsset::Found {
                offline: true,
                path_matches: true,
            },
            OldAsset::Found {
                offline: true,
                path_matches: false,
            },
        ] {
            let back = Facts {
                disk: Disk::Present,
                old,
                ..facts()
            };
            assert_eq!(decide(&back), Verdict::Superseded, "{old:?}");
        }
    }

    #[test]
    fn an_unreadable_old_path_never_leads_to_a_delete() {
        for new in [
            NewSide::Empty,
            NewSide::Unmapped,
            NewSide::Missing,
            NewSide::Importing,
            NewSide::Ready,
        ] {
            let unknown = Facts {
                disk: Disk::Unknown,
                new,
                ..facts()
            };
            assert_eq!(decide(&unknown), Verdict::Unreadable, "{new:?}");
        }
    }

    #[test]
    fn an_asset_immich_does_not_know_is_gone() {
        let gone = Facts {
            old: OldAsset::Missing,
            ..facts()
        };
        assert_eq!(decide(&gone), Verdict::Gone);
    }

    #[test]
    fn an_asset_that_followed_the_file_by_itself_needs_nothing() {
        let followed = Facts {
            old: OldAsset::Moved,
            ..facts()
        };
        assert_eq!(decide(&followed), Verdict::Superseded);
        let unseen = Facts {
            offline_seen_at: None,
            ..followed
        };
        assert_eq!(decide(&unseen), Verdict::Superseded);
    }

    #[test]
    fn an_asset_at_another_path_is_not_ours_to_delete() {
        for offline in [true, false] {
            let other = Facts {
                old: OldAsset::Found {
                    offline,
                    path_matches: false,
                },
                ..facts()
            };
            assert_eq!(decide(&other), Verdict::PathMismatch, "offline: {offline}");
        }
    }

    #[test]
    fn an_online_asset_is_never_deleted() {
        for new in [
            NewSide::Empty,
            NewSide::Unmapped,
            NewSide::Missing,
            NewSide::Importing,
            NewSide::Ready,
        ] {
            let waiting = Facts {
                old: online(),
                new,
                offline_seen_at: None,
                ..facts()
            };
            assert_eq!(
                decide(&waiting),
                Verdict::Wait {
                    first_offline: false
                },
                "{new:?}"
            );
        }
    }

    #[test]
    fn an_asset_that_was_offline_and_is_online_again_is_superseded() {
        let back = Facts {
            old: online(),
            offline_seen_at: Some(NOW - 600),
            ..facts()
        };
        assert_eq!(decide(&back), Verdict::Superseded);
    }

    #[test]
    fn an_importing_new_asset_is_waited_for_however_long_it_takes() {
        let importing = Facts {
            new: NewSide::Importing,
            offline_seen_at: Some(NOW - 100 * NEW_ASSET_GRACE_SECS),
            ..facts()
        };
        assert_eq!(
            decide(&importing),
            Verdict::Wait {
                first_offline: false
            }
        );
    }

    #[test]
    fn the_first_pass_that_sees_the_old_asset_offline_records_it() {
        let fresh = Facts {
            new: NewSide::Missing,
            offline_seen_at: None,
            ..facts()
        };
        assert_eq!(
            decide(&fresh),
            Verdict::Wait {
                first_offline: true
            }
        );
        let importing = Facts {
            new: NewSide::Importing,
            offline_seen_at: None,
            ..facts()
        };
        assert_eq!(
            decide(&importing),
            Verdict::Wait {
                first_offline: true
            }
        );
    }

    #[test]
    fn without_a_new_asset_the_old_one_goes_after_the_grace_only() {
        let waiting = |seen| Facts {
            new: NewSide::Missing,
            offline_seen_at: Some(seen),
            ..facts()
        };
        assert_eq!(
            decide(&waiting(NOW - NEW_ASSET_GRACE_SECS + 1)),
            Verdict::Wait {
                first_offline: false
            }
        );
        assert_eq!(
            decide(&waiting(NOW - NEW_ASSET_GRACE_SECS)),
            Verdict::Delete { carry: false }
        );
        assert_eq!(
            decide(&waiting(NOW - 10 * NEW_ASSET_GRACE_SECS)),
            Verdict::Delete { carry: false }
        );
    }

    /// The safety property in one place: whatever the facts, a delete needs proof that the file is
    /// gone, an offline asset at the expected path.
    #[test]
    fn a_delete_always_has_its_three_proofs() {
        let disks = [Disk::Absent, Disk::Present, Disk::Unknown];
        let olds = [
            OldAsset::Missing,
            OldAsset::Moved,
            OldAsset::Found {
                offline: true,
                path_matches: true,
            },
            OldAsset::Found {
                offline: true,
                path_matches: false,
            },
            OldAsset::Found {
                offline: false,
                path_matches: true,
            },
            OldAsset::Found {
                offline: false,
                path_matches: false,
            },
        ];
        let news = [
            NewSide::Empty,
            NewSide::Unmapped,
            NewSide::Missing,
            NewSide::Importing,
            NewSide::Ready,
        ];
        let seen = [None, Some(NOW - 1), Some(NOW - 10 * NEW_ASSET_GRACE_SECS)];
        for disk in disks {
            for old in olds {
                for new in news {
                    for offline_seen_at in seen {
                        for carried in [false, true] {
                            let facts = Facts {
                                disk,
                                old,
                                new,
                                carried,
                                offline_seen_at,
                                now: NOW,
                            };
                            if matches!(decide(&facts), Verdict::Delete { .. }) {
                                assert_eq!(disk, Disk::Absent, "{facts:?}");
                                assert_eq!(
                                    old,
                                    OldAsset::Found {
                                        offline: true,
                                        path_matches: true
                                    },
                                    "{facts:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
