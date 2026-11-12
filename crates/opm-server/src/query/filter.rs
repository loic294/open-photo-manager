//! The WHERE clause: scope, kind and filter as one boolean SQL expression.
//!
//! [`FileQuery::push_where`] is the public entry point; the crate-private pieces below are what
//! the folder list and the facets compose differently (the filter alone, or every dimension but
//! one).

use opm_core::dto::FileKind;
use sqlx::{Encode, QueryBuilder, Sqlite, Type};

use super::{FileQuery, KindFilter};
use crate::db::models::{file_kind_to_db, flag_to_db};
use crate::db::subtree::Subtree;

/// A filter dimension that a facet leaves out when it counts its own values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dimension {
    Cameras,
    Ratings,
    Tags,
}

/// How much of the filter a condition carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FilterPart {
    /// Every dimension.
    All,
    /// Every dimension but one: what a facet counts.
    Except(Dimension),
    /// None: the caller applies the filter itself (the folder list counts both ways).
    Nothing,
}

/// A parenthesised `AND` chain: `(a AND b)`, or `(1 = 1)` when no term was added.
pub(super) struct And<'a> {
    qb: &'a mut QueryBuilder<Sqlite>,
    empty: bool,
}

impl<'a> And<'a> {
    pub(super) fn open(qb: &'a mut QueryBuilder<Sqlite>) -> Self {
        qb.push("(");
        Self { qb, empty: true }
    }

    /// Starts the next term (after the `AND` when it is not the first) and returns the builder to
    /// write it into.
    fn term(&mut self) -> &mut QueryBuilder<Sqlite> {
        if !self.empty {
            self.qb.push(" AND ");
        }
        self.empty = false;
        self.qb
    }

    pub(super) fn close(self) {
        if self.empty {
            self.qb.push("1 = 1");
        }
        self.qb.push(")");
    }
}

/// ` IN (?, ?, ...)` with one bind per value; `values` must not be empty.
fn push_in_list<'t, T>(qb: &mut QueryBuilder<Sqlite>, values: impl IntoIterator<Item = T>)
where
    T: Encode<'t, Sqlite> + Type<Sqlite>,
{
    qb.push(" IN (");
    let mut list = qb.separated(", ");
    for value in values {
        list.push_bind(value);
    }
    list.push_unseparated(")");
}

impl FileQuery {
    /// Appends the condition "this row is in the scope, has an allowed kind and passes the
    /// filter" for the `files` table aliased `alias` (the other modules use `f`; see the module
    /// docs of [`crate::query`] for how to embed it).
    ///
    /// It is one parenthesised boolean expression and never empty (`(1 = 1)` when nothing
    /// constrains), so it can follow `WHERE` or `AND`, sit inside `CASE WHEN ... END`, and appear
    /// in `INSERT ... SELECT`, `UPDATE` and `DELETE` statements. Every value is bound; `alias` is
    /// spliced into the SQL, so pass a literal, never user input.
    pub fn push_where(&self, qb: &mut QueryBuilder<Sqlite>, alias: &str) {
        self.push_condition(qb, alias, self.scope.recursive, FilterPart::All);
    }

    /// [`Self::push_where`] with the scope's `recursive` flag overridden and `filter` choosing how
    /// much of the filter applies.
    pub(super) fn push_condition(
        &self,
        qb: &mut QueryBuilder<Sqlite>,
        alias: &str,
        recursive: bool,
        filter: FilterPart,
    ) {
        let mut and = And::open(qb);
        self.push_scope(&mut and, alias, recursive);
        self.push_kind(&mut and, alias);
        match filter {
            FilterPart::All => self.push_filter(&mut and, alias, None),
            FilterPart::Except(dimension) => self.push_filter(&mut and, alias, Some(dimension)),
            FilterPart::Nothing => {}
        }
        and.close();
    }

    /// The scope alone: the volume, and the directory (equal, or the subtree index range).
    fn push_scope(&self, and: &mut And<'_>, alias: &str, recursive: bool) {
        let subtree = Subtree::new(&self.scope.dir);
        let constrains_dir = !(recursive && subtree.is_root());
        match self.scope.volume_id {
            Some(volume_id) => {
                and.term()
                    .push(alias)
                    .push(".volume_id = ")
                    .push_bind(volume_id);
            }
            // "Every volume" is stated as a membership test so that the planner can still search
            // `(volume_id, dir_path, name)` once per volume instead of scanning the whole table.
            None if constrains_dir => {
                and.term()
                    .push(alias)
                    .push(".volume_id IN (SELECT id FROM volumes)");
            }
            None => {}
        }
        let column = format!("{alias}.dir_path");
        if !recursive {
            and.term()
                .push(&column)
                .push(" = ")
                .push_bind(self.scope.dir.as_str().to_owned());
        } else if constrains_dir {
            subtree.push_predicate(and.term(), &column);
        }
    }

    fn push_kind(&self, and: &mut And<'_>, alias: &str) {
        match self.kinds {
            KindFilter::Assets => {
                and.term()
                    .push(alias)
                    .push(".kind <> ")
                    .push_bind(file_kind_to_db(FileKind::Sidecar));
            }
            KindFilter::All => {}
        }
    }

    /// The filter's dimensions, except `skip`. The flag dimension is never skipped (there is no
    /// flag facet) and the selection always restricts.
    fn push_filter(&self, and: &mut And<'_>, alias: &str, skip: Option<Dimension>) {
        let filter = &self.filter;
        if skip != Some(Dimension::Cameras) && !filter.cameras.is_empty() {
            let qb = and.term();
            qb.push(alias)
                .push(".camera_id IN (SELECT id FROM cameras WHERE name");
            push_in_list(qb, filter.cameras.iter().cloned());
            qb.push(")");
        }
        if skip != Some(Dimension::Ratings) && !filter.ratings.is_empty() {
            let qb = and.term();
            qb.push(alias).push(".rating");
            push_in_list(qb, filter.ratings.iter().map(|&rating| i64::from(rating)));
        }
        if !filter.flags.is_empty() {
            let qb = and.term();
            qb.push(alias).push(".flag");
            push_in_list(qb, filter.flags.iter().map(|&flag| flag_to_db(Some(flag))));
        }
        if skip != Some(Dimension::Tags) && !filter.tags.is_empty() {
            let qb = and.term();
            qb.push("EXISTS (SELECT 1 FROM file_tags WHERE file_tags.file_id = ")
                .push(alias)
                .push(".id AND file_tags.tag_id IN (SELECT id FROM tags WHERE name");
            push_in_list(qb, filter.tags.iter().cloned());
            qb.push("))");
        }
        if let Some(selection_id) = filter.selection_id {
            and.term()
                .push(alias)
                .push(".id IN (SELECT file_id FROM selection_items WHERE selection_id = ")
                .push_bind(selection_id)
                .push(")");
        }
    }

    /// The filter alone as one parenthesised expression: no scope, no kind.
    pub(super) fn push_filter_only(&self, qb: &mut QueryBuilder<Sqlite>, alias: &str) {
        let mut and = And::open(qb);
        self.push_filter(&mut and, alias, None);
        and.close();
    }

    /// Whether the filter constrains nothing.
    pub(super) fn filter_is_empty(&self) -> bool {
        self.filter.is_empty()
    }
}
