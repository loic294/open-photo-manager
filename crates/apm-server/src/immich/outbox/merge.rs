//! How a new change of a file folds into the change that is still waiting for it.
//!
//! Several operations on one file can pile up before the worker runs (a move, a rename, a trash).
//! Replaying them one by one would wait for evidence Immich can never give: the file that moved
//! from A to B and then from B to C never exists at B for a library scan. So, as long as the scan
//! was not requested for a waiting row, a new change of the same file MERGES into it: the old
//! side stays the FIRST old place, the new side becomes the LAST new place. A row whose two sides
//! end up empty or equal is dropped: the file is where Immich already has it.
//!
//! Pure: no database, no clock. The hook (`hook`) feeds it, `store` applies its answer.

use apm_core::paths::VolumeId;

/// A place inside a volume, as a row of the outbox stores it. The strings are the exact on-disk
/// names: never normalised.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Place {
    pub volume_id: VolumeId,
    /// The folder, `""` for the volume root.
    pub dir: String,
    pub name: String,
}

impl Place {
    pub fn new(volume_id: VolumeId, dir: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            volume_id,
            dir: dir.into(),
            name: name.into(),
        }
    }
}

/// What happened to a file: where it was and where it is now.
///
/// A move has both sides, a trash only `old`, a copy or a restore only `new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effect {
    pub old: Option<Place>,
    pub new: Option<Place>,
}

impl Effect {
    #[cfg(test)]
    pub fn moved(old: Place, new: Place) -> Self {
        Self {
            old: Some(old),
            new: Some(new),
        }
    }

    pub fn removed(old: Place) -> Self {
        Self {
            old: Some(old),
            new: None,
        }
    }

    pub fn added(new: Place) -> Self {
        Self {
            old: None,
            new: Some(new),
        }
    }

    /// Nothing for Immich to learn: no side at all, or the same place on both sides.
    pub fn is_noop(&self) -> bool {
        match (&self.old, &self.new) {
            (None, None) => true,
            (Some(old), Some(new)) => old == new,
            _ => false,
        }
    }
}

/// What to do with the waiting row after a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Merged {
    /// There is nothing to reflect: delete the waiting row, or write none.
    Drop,
    /// Keep exactly this change: update the waiting row, or insert one.
    Replace(Effect),
}

/// Folds `incoming` into the `waiting` change of the same file (`None`: no row is waiting).
pub fn merge(waiting: Option<&Effect>, incoming: &Effect) -> Merged {
    let folded = Effect {
        old: waiting.map_or_else(|| incoming.old.clone(), |row| row.old.clone()),
        new: incoming.new.clone(),
    };
    if folded.is_noop() {
        Merged::Drop
    } else {
        Merged::Replace(folded)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn at(name: &str) -> Place {
        Place::new(1, "d", name)
    }

    fn fold(effects: &[Effect]) -> Option<Effect> {
        let mut waiting: Option<Effect> = None;
        for effect in effects {
            waiting = match merge(waiting.as_ref(), effect) {
                Merged::Drop => None,
                Merged::Replace(row) => Some(row),
            };
        }
        waiting
    }

    #[test]
    fn a_change_with_nothing_waiting_is_kept() {
        let effect = Effect::moved(at("a"), at("b"));
        assert_eq!(merge(None, &effect), Merged::Replace(effect));
        let added = Effect::added(at("a"));
        assert_eq!(merge(None, &added), Merged::Replace(added));
        let removed = Effect::removed(at("a"));
        assert_eq!(merge(None, &removed), Merged::Replace(removed));
    }

    #[test]
    fn a_change_that_changes_nothing_is_dropped_even_without_a_waiting_row() {
        assert_eq!(merge(None, &Effect::moved(at("a"), at("a"))), Merged::Drop);
        assert_eq!(
            merge(
                None,
                &Effect {
                    old: None,
                    new: None
                }
            ),
            Merged::Drop
        );
    }

    #[test]
    fn a_to_b_then_b_to_c_is_a_to_c() {
        let rows = [
            Effect::moved(at("a"), at("b")),
            Effect::moved(at("b"), at("c")),
        ];
        assert_eq!(fold(&rows), Some(Effect::moved(at("a"), at("c"))));
    }

    #[test]
    fn a_to_b_then_trash_b_is_remove_a() {
        let rows = [Effect::moved(at("a"), at("b")), Effect::removed(at("b"))];
        assert_eq!(fold(&rows), Some(Effect::removed(at("a"))));
    }

    #[test]
    fn restore_x_then_move_x_to_y_is_add_y() {
        let rows = [Effect::added(at("x")), Effect::moved(at("x"), at("y"))];
        assert_eq!(fold(&rows), Some(Effect::added(at("y"))));
    }

    #[test]
    fn a_to_b_then_b_to_a_is_dropped() {
        let rows = [
            Effect::moved(at("a"), at("b")),
            Effect::moved(at("b"), at("a")),
        ];
        assert_eq!(fold(&rows), None);
    }

    #[test]
    fn added_then_trashed_is_dropped() {
        let rows = [Effect::added(at("x")), Effect::removed(at("x"))];
        assert_eq!(fold(&rows), None);
    }

    #[test]
    fn moving_again_after_a_dropped_round_trip_starts_afresh() {
        let rows = [
            Effect::moved(at("a"), at("b")),
            Effect::moved(at("b"), at("a")),
            Effect::moved(at("a"), at("c")),
        ];
        assert_eq!(fold(&rows), Some(Effect::moved(at("a"), at("c"))));
    }

    #[test]
    fn places_in_other_volumes_or_folders_are_different_places() {
        let here = Place::new(1, "d", "a");
        let elsewhere = Place::new(2, "d", "a");
        let deeper = Place::new(1, "d/e", "a");
        assert!(!Effect::moved(here.clone(), elsewhere).is_noop());
        assert!(!Effect::moved(here.clone(), deeper).is_noop());
        assert!(Effect::moved(here.clone(), here).is_noop());
    }

    #[test]
    fn names_are_compared_byte_for_byte() {
        let lower = at("photo.jpg");
        let upper = at("PHOTO.JPG");
        assert!(
            !Effect::moved(lower, upper).is_noop(),
            "a case rename is a change"
        );
    }

    fn place() -> impl Strategy<Value = Place> {
        (
            1_i64..3,
            prop::sample::select(vec!["", "a", "a/b"]),
            "[a-c]{1,2}",
        )
            .prop_map(|(volume, dir, name)| Place::new(volume, dir, name))
    }

    /// A chain of effects on one file: each one starts where the previous one ended.
    fn chain() -> impl Strategy<Value = Vec<Effect>> {
        (
            prop::option::of(place()),
            prop::collection::vec(prop::option::weighted(0.8, place()), 1..8),
        )
            .prop_map(|(start, steps)| {
                let mut at = start;
                steps
                    .into_iter()
                    .map(|next| {
                        let effect = Effect {
                            old: at.clone(),
                            new: next.clone(),
                        };
                        at = next;
                        effect
                    })
                    .collect()
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// The oracle: the first old place and the last new place, nothing if they say nothing.
        #[test]
        fn merging_a_chain_keeps_the_first_old_and_the_last_new(effects in chain()) {
            let first = effects.first().map(|effect| effect.old.clone()).unwrap_or(None);
            let last = effects.last().map(|effect| effect.new.clone()).unwrap_or(None);
            let expected = Effect { old: first, new: last };
            let expected = (!expected.is_noop()).then_some(expected);
            prop_assert_eq!(fold(&effects), expected);
        }

        /// Merging never invents a place: every side comes from the effects that were given.
        #[test]
        fn a_merged_row_only_holds_places_it_was_given(effects in chain()) {
            if let Some(row) = fold(&effects) {
                if let Some(old) = &row.old {
                    prop_assert!(effects.iter().any(|effect| effect.old.as_ref() == Some(old)));
                }
                if let Some(new) = &row.new {
                    prop_assert!(effects.iter().any(|effect| effect.new.as_ref() == Some(new)));
                }
            }
        }
    }
}
