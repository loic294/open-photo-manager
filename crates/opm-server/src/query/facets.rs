//! Facets: the values of the camera, tag and rating dimensions with their counts.

use opm_core::dto::{FacetCount, Facets};
use sqlx::{QueryBuilder, Sqlite};

use super::filter::{Dimension, FilterPart};
use super::{FileQuery, MAX_FACET_VALUES, QueryError};
use crate::db::Db;

impl FileQuery {
    /// The cameras, tags and ratings of the files in the scope, with how many files carry each.
    ///
    /// A dimension is counted under every filter EXCEPT its own, so the UI can offer the
    /// alternatives to what is selected (the other dimensions and the selection still restrict).
    /// Each list is sorted by count, highest first, then by value, and holds at most
    /// [`MAX_FACET_VALUES`] entries. Ratings are `"0"` (unrated) to `"5"` and only the values that
    /// occur are listed; files without a camera count for no camera.
    pub async fn facets(&self, db: &Db) -> Result<Facets, QueryError> {
        let (cameras, tags, ratings) = tokio::try_join!(
            named_counts(db, self.camera_facet_statement()),
            named_counts(db, self.tag_facet_statement()),
            rating_counts(db, self.rating_facet_statement()),
        )?;
        Ok(Facets {
            cameras,
            tags,
            ratings,
        })
    }

    pub(super) fn camera_facet_statement(&self) -> QueryBuilder<Sqlite> {
        let mut qb = QueryBuilder::new(
            "SELECT cam.name, COUNT(*) AS n FROM files f \
             JOIN cameras cam ON cam.id = f.camera_id WHERE ",
        );
        self.push_condition(
            &mut qb,
            "f",
            self.scope.recursive,
            FilterPart::Except(Dimension::Cameras),
        );
        qb.push(" GROUP BY cam.id ORDER BY n DESC, cam.name LIMIT ")
            .push_bind(MAX_FACET_VALUES);
        qb
    }

    pub(super) fn tag_facet_statement(&self) -> QueryBuilder<Sqlite> {
        let mut qb = QueryBuilder::new(
            "SELECT t.name, COUNT(*) AS n FROM files f \
             JOIN file_tags ft ON ft.file_id = f.id JOIN tags t ON t.id = ft.tag_id WHERE ",
        );
        self.push_condition(
            &mut qb,
            "f",
            self.scope.recursive,
            FilterPart::Except(Dimension::Tags),
        );
        qb.push(" GROUP BY t.id ORDER BY n DESC, t.name LIMIT ")
            .push_bind(MAX_FACET_VALUES);
        qb
    }

    pub(super) fn rating_facet_statement(&self) -> QueryBuilder<Sqlite> {
        let mut qb = QueryBuilder::new("SELECT f.rating, COUNT(*) AS n FROM files f WHERE ");
        self.push_condition(
            &mut qb,
            "f",
            self.scope.recursive,
            FilterPart::Except(Dimension::Ratings),
        );
        qb.push(" GROUP BY f.rating ORDER BY n DESC, f.rating");
        qb
    }
}

/// Runs a statement that selects `(name, count)` pairs.
async fn named_counts(
    db: &Db,
    mut statement: QueryBuilder<Sqlite>,
) -> Result<Vec<FacetCount>, QueryError> {
    let rows: Vec<(String, i64)> = statement.build_query_as().fetch_all(db.read()).await?;
    Ok(rows
        .into_iter()
        .map(|(value, count)| FacetCount { value, count })
        .collect())
}

/// Runs a statement that selects `(rating, count)` pairs; the value is the rating as text.
async fn rating_counts(
    db: &Db,
    mut statement: QueryBuilder<Sqlite>,
) -> Result<Vec<FacetCount>, QueryError> {
    let rows: Vec<(i64, i64)> = statement.build_query_as().fetch_all(db.read()).await?;
    Ok(rows
        .into_iter()
        .map(|(rating, count)| FacetCount {
            value: rating.to_string(),
            count,
        })
        .collect())
}
