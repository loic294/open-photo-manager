use std::collections::{HashMap, HashSet};

use apm_core::dto::{DuplicateParams, MatchMode, SideParams};
use apm_core::matching::{Overlap, Pair, is_valid_pair, stem_prefix_match};
use apm_core::paths::FileId;
use futures_util::{TryStreamExt, future::BoxFuture};
use serde_json::Value;
use sqlx::{QueryBuilder, Sqlite, SqliteConnection};
use tokio_util::sync::CancellationToken;

use super::{MAX_PAIRS, OverlapPolicy as Policy, PairVisitor as Visitor};
use crate::plans::{BuildContext, Built, BulkWriter, PlanError, bulk};
use crate::query::{FileQuery, QueryScope};

#[cfg(test)]
mod property_tests;

/// Move uses Unoriented because preserving destination structure is not a delete operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverlapPolicy {
    Oriented,
    // Reserved for the downstream move planner; matching property tests cover this branch.
    #[allow(dead_code)]
    Unoriented,
}

/// Backpressure-capable visitor: never collect the whole source x comparison product.
pub(crate) trait PairVisitor: Send {
    fn visit(&mut self, pair: Pair) -> BoxFuture<'_, Result<(), PlanError>>;
}

pub(super) fn query(side: &SideParams, label: &str) -> Result<FileQuery, PlanError> {
    let scope = QueryScope::try_from(&side.scope)
        .map_err(|err| PlanError::invalid_field(format!("{label}.scope"), err.to_string()))?;
    FileQuery::new(scope, &side.filter)
        .map_err(|err| PlanError::invalid_field(format!("{label}.filter"), err.to_string()))
}

fn predicate(qb: &mut QueryBuilder<Sqlite>, query: &FileQuery, alias: &str) {
    query.push_where(qb, alias);
    qb.push(" AND ")
        .push(alias)
        .push(".kind IN ('image', 'video', 'audio')");
}

pub(super) fn join_statement(
    mode: MatchMode,
    source: &FileQuery,
    comparison: &FileQuery,
) -> QueryBuilder<Sqlite> {
    let mut qb = QueryBuilder::new(
        "SELECT s.id, c.id FROM (SELECT f.id, f.hash, f.name_norm, f.capture_ts FROM files f WHERE ",
    );
    predicate(&mut qb, source, "f");
    // CROSS JOIN fixes loop order so the comparison uses the match-key index, not a rescan.
    qb.push(
        ") s CROSS JOIN (SELECT f.id, f.hash, f.name_norm, f.capture_ts FROM files f INDEXED BY ",
    )
    .push(if mode == MatchMode::Hash {
        "files_hash"
    } else {
        "files_name_capture"
    })
    .push(" WHERE ");
    predicate(&mut qb, comparison, "f");
    if mode == MatchMode::Hash {
        qb.push(" AND f.hash IS NOT NULL");
    }
    qb.push(") c WHERE s.id <> c.id AND ");
    qb.push(match mode {
        MatchMode::Hash => "c.hash = s.hash",
        MatchMode::NameAndTime => "c.name_norm = s.name_norm AND c.capture_ts = s.capture_ts",
        MatchMode::NameDifferentTime => {
            "c.name_norm = s.name_norm AND c.capture_ts IS NOT s.capture_ts"
        }
        MatchMode::NamePrefix => "0",
    });
    qb
}

fn cancelled(cancel: &CancellationToken) -> Result<(), PlanError> {
    if cancel.is_cancelled() {
        Err(PlanError::Cancelled)
    } else {
        Ok(())
    }
}

async fn overlap(
    conn: &mut SqliteConnection,
    source: &FileQuery,
    comparison: &FileQuery,
    cancel: &CancellationToken,
) -> Result<HashSet<FileId>, PlanError> {
    let mut qb = QueryBuilder::new("SELECT f.id FROM files f WHERE ");
    predicate(&mut qb, source, "f");
    qb.push(" AND ");
    predicate(&mut qb, comparison, "f");
    let mut rows = qb.build_query_scalar::<FileId>().fetch(conn);
    let mut ids = HashSet::new();
    while let Some(id) = rows.try_next().await? {
        cancelled(cancel)?;
        ids.insert(id);
    }
    Ok(ids)
}

/// Streams pairs on the caller's read snapshot. Both sides use the query engine's scope/filter.
/// Returns the overlap size; the visitor can impose its own caps and write on the DB writer.
pub(crate) async fn candidate_pairs(
    conn: &mut SqliteConnection,
    params: &DuplicateParams,
    policy: Policy,
    cancel: &CancellationToken,
    visitor: &mut dyn Visitor,
) -> Result<u64, PlanError> {
    let mode = params.mode;
    let strict_prefix = params.strict_prefix;
    let source = query(&params.source, "source")?;
    let comparison = query(&params.comparison, "comparison")?;
    let both = if policy == Policy::Oriented {
        overlap(conn, &source, &comparison, cancel).await?
    } else {
        HashSet::new()
    };
    let valid = |pair: Pair| match policy {
        Policy::Unoriented => pair.source != pair.comparison,
        Policy::Oriented => is_valid_pair(
            pair,
            Overlap {
                source_in_comparison: both.contains(&pair.source),
                comparison_in_source: both.contains(&pair.comparison),
            },
        ),
    };
    if mode == MatchMode::NamePrefix {
        let mut qb = QueryBuilder::new("SELECT f.id, f.stem_norm FROM files f WHERE ");
        predicate(&mut qb, &source, "f");
        let mut stems: HashMap<String, Vec<FileId>> = HashMap::new();
        {
            let mut rows = qb.build_query_as::<(FileId, String)>().fetch(&mut *conn);
            while let Some((id, stem)) = rows.try_next().await? {
                cancelled(cancel)?;
                if !stem.is_empty() {
                    stems.entry(stem).or_default().push(id);
                }
            }
        }
        let mut qb = QueryBuilder::new("SELECT f.id, f.stem_norm FROM files f WHERE ");
        predicate(&mut qb, &comparison, "f");
        let mut rows = qb.build_query_as::<(FileId, String)>().fetch(&mut *conn);
        while let Some((id, stem)) = rows.try_next().await? {
            cancelled(cancel)?;
            for end in stem
                .char_indices()
                .map(|(index, _)| index)
                .skip(1)
                .chain(std::iter::once(stem.len()))
            {
                let prefix = &stem[..end];
                if let Some(ids) = stems.get(prefix)
                    && stem_prefix_match(prefix, &stem, strict_prefix)
                {
                    for source in ids {
                        cancelled(cancel)?;
                        let pair = Pair::new(*source, id);
                        if valid(pair) {
                            visitor.visit(pair).await?;
                        }
                    }
                }
            }
        }
    } else {
        let mut qb = join_statement(mode, &source, &comparison);
        let mut rows = qb.build_query_as::<(FileId, FileId)>().fetch(conn);
        while let Some((source, comparison)) = rows.try_next().await? {
            cancelled(cancel)?;
            let pair = Pair::new(source, comparison);
            if valid(pair) {
                visitor.visit(pair).await?;
            }
        }
    }
    Ok(both.len() as u64)
}

struct Sink<W> {
    writer: W,
    source: HashSet<FileId>,
    comparison: HashSet<FileId>,
    pairs: u64,
    max_pairs: u64,
    max_matched: usize,
}

impl<F> Visitor for Sink<BulkWriter<Pair, F>>
where
    F: FnMut(sqlx::query_builder::Separated<'_, Sqlite, &'static str>, Pair) + Send,
{
    fn visit(&mut self, pair: Pair) -> BoxFuture<'_, Result<(), PlanError>> {
        Box::pin(async move {
            self.pairs += 1;
            if self.pairs > self.max_pairs {
                return Err(PlanError::TooLarge {
                    items: self.pairs,
                    max: self.max_pairs,
                });
            }
            self.source.insert(pair.source);
            self.comparison.insert(pair.comparison);
            let matched = self.source.len().max(self.comparison.len());
            if matched > self.max_matched {
                return Err(PlanError::TooLarge {
                    items: matched as u64,
                    max: self.max_matched as u64,
                });
            }
            self.writer.push(pair).await
        })
    }
}

async fn counts(
    conn: &mut SqliteConnection,
    side: &SideParams,
    mode: MatchMode,
    label: &str,
) -> Result<(u64, u64), PlanError> {
    let query = query(side, label)?;
    let missing = match mode {
        MatchMode::Hash => "f.hash IS NULL",
        MatchMode::NameAndTime => "f.capture_ts IS NULL",
        MatchMode::NamePrefix => "f.stem_norm = ''",
        MatchMode::NameDifferentTime => "0",
    };
    let mut qb = QueryBuilder::new("SELECT COUNT(*), COALESCE(SUM(");
    qb.push(missing).push("), 0) FROM files f WHERE ");
    predicate(&mut qb, &query, "f");
    let (count, missing): (i64, i64) = qb.build_query_as().fetch_one(conn).await?;
    Ok((count as u64, missing as u64))
}

pub(super) async fn build(
    ctx: &BuildContext,
    value: Value,
    max_matched: usize,
) -> Result<Built, PlanError> {
    let params: DuplicateParams =
        serde_json::from_value(value).map_err(|err| PlanError::invalid(err.to_string()))?;
    ctx.check_cancelled()?;
    let mut registration = ctx.db.begin_write().await?;
    crate::plans::file_identity::begin(&mut registration, ctx.plan_id).await?;
    registration.commit().await?;
    let mut read = ctx.db.read().begin().await?;
    let (source_count, source_missing) =
        counts(&mut read, &params.source, params.mode, "source").await?;
    let (comparison_count, comparison_missing) =
        counts(&mut read, &params.comparison, params.mode, "comparison").await?;
    let id = ctx.plan_id;
    let writer = BulkWriter::new(
        &ctx.db,
        &ctx.cancel,
        "duplicate_pairs",
        &["plan_id", "source_id", "comparison_id"],
        move |mut row, pair: Pair| {
            row.push_bind(id)
                .push_bind(pair.source)
                .push_bind(pair.comparison);
        },
    )?;
    let mut sink = Sink {
        writer,
        source: HashSet::new(),
        comparison: HashSet::new(),
        pairs: 0,
        max_pairs: ctx.max_items.min(MAX_PAIRS),
        max_matched,
    };
    let overlap =
        super::candidate_pairs(&mut read, &params, Policy::Oriented, &ctx.cancel, &mut sink)
            .await?;
    sink.writer.finish().await?;
    let mut built = Built::new(sink.pairs)
        .stat("source_candidates", source_count)
        .stat("comparison_candidates", comparison_count)
        .stat("pairs", sink.pairs)
        .stat("source_matched", sink.source.len() as u64)
        .stat("comparison_matched", sink.comparison.len() as u64)
        .stat("unmatchable_source", source_missing)
        .stat("unmatchable_comparison", comparison_missing);
    for (side, ids) in [("source", sink.source), ("comparison", sink.comparison)] {
        let ids: Vec<_> = ids.into_iter().collect();
        let mut bytes = 0_i64;
        let mut members = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(30_000) {
            let mut qb = QueryBuilder::<Sqlite>::new(
                "SELECT id, volume_id, dir_path, size FROM files WHERE id IN (",
            );
            let mut separated = qb.separated(", ");
            for id in chunk {
                separated.push_bind(*id);
            }
            separated.push_unseparated(")");
            for (id, volume, dir, size) in qb
                .build_query_as::<(i64, i64, String, i64)>()
                .fetch_all(&mut *read)
                .await?
            {
                bytes += size;
                members.push((id, volume, dir));
            }
        }
        for chunk in ids.chunks(bulk::CHUNK_ROWS) {
            ctx.check_cancelled()?;
            let mut write = ctx.db.begin_write().await?;
            crate::plans::file_identity::track(&mut write, id, chunk).await?;
            write.commit().await?;
        }
        bulk::insert_all(
            &ctx.db,
            &ctx.cancel,
            "duplicate_matches",
            &["plan_id", "side", "file_id", "volume_id", "dir_path"],
            members,
            move |mut row, (file_id, volume, dir)| {
                row.push_bind(id)
                    .push_bind(side)
                    .push_bind(file_id)
                    .push_bind(volume)
                    .push_bind(dir);
            },
        )
        .await?;
        built = built.stat(&format!("{side}_matched_bytes"), bytes as u64);
    }
    read.commit().await?;
    let (code, message) = match params.mode {
        MatchMode::Hash => (
            "unhashed_files",
            "Files are not hashed yet; re-index hashes to include them",
        ),
        MatchMode::NameAndTime => (
            "no_capture_time",
            "Files without a capture time cannot match in filename and time mode",
        ),
        MatchMode::NamePrefix => (
            "empty_stem",
            "Files with an empty filename stem cannot match prefixes",
        ),
        MatchMode::NameDifferentTime => ("", ""),
    };
    ctx.check_cancelled()?;
    Ok(built.warning(code, message, source_missing + comparison_missing)
        .warning("overlapping_scopes", "The scopes overlap; pairs are oriented and deletion requires a survivor outside the deletion set", overlap))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        Db,
        testing::{insert_file, insert_volume},
    };
    use apm_core::{
        dto::{Filter, Scope},
        matching::{MatchFacts, MatchOptions, is_match},
    };
    use sqlx::Row;

    #[derive(Default)]
    struct Collect(HashSet<Pair>);
    impl PairVisitor for Collect {
        fn visit(&mut self, pair: Pair) -> BoxFuture<'_, Result<(), PlanError>> {
            self.0.insert(pair);
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn both_policies_match_reference_and_exact_joins_search_comparison_indexes() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let volume = insert_volume(&db, "v").await;
        let names = [
            "one.jpg",
            "ONE.JPG",
            "one_edit.jpg",
            "one2.jpg",
            "other.jpg",
        ];
        let mut facts = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let id = insert_file(&db, volume, &format!("d{index}"), name).await;
            let capture = if index % 3 == 0 {
                None
            } else {
                Some((index % 2) as i64)
            };
            let hash = if index % 3 == 0 {
                None
            } else {
                Some(apm_core::hash::sha1_bytes(&[(index % 2) as u8]))
            };
            sqlx::query("UPDATE files SET kind = 'image', name_norm = ?, stem_norm = ?, capture_ts = ?, hash = ? WHERE id = ?")
                            .bind(apm_core::names::normalize_name(name)).bind(apm_core::names::stem_norm(name)).bind(capture).bind(hash.map(|h| h.to_vec())).bind(id)
                            .execute(db.write()).await.unwrap();
            facts.push((id, *name, hash, capture));
        }
        let side = SideParams {
            scope: Scope {
                volume_id: Some(volume),
                dir: String::new(),
                recursive: true,
            },
            filter: Filter::default(),
        };
        let cancel = CancellationToken::new();
        for mode in MatchMode::ALL {
            for strict_prefix in [false, true] {
                for policy in [Policy::Oriented, Policy::Unoriented] {
                    let params = DuplicateParams {
                        mode,
                        strict_prefix,
                        source: side.clone(),
                        comparison: side.clone(),
                    };
                    let mut collect = Collect::default();
                    let mut conn = db.read().acquire().await.unwrap();
                    candidate_pairs(&mut conn, &params, policy, &cancel, &mut collect)
                        .await
                        .unwrap();
                    let mut expected = HashSet::new();
                    for s in &facts {
                        for c in &facts {
                            let sf = MatchFacts {
                                id: s.0,
                                name: s.1,
                                hash: s.2,
                                capture_ts: s.3,
                                size: 0,
                            };
                            let cf = MatchFacts {
                                id: c.0,
                                name: c.1,
                                hash: c.2,
                                capture_ts: c.3,
                                size: 0,
                            };
                            let pair = Pair::new(s.0, c.0);
                            if is_match(mode, &sf, &cf, MatchOptions { strict_prefix })
                                && (policy == Policy::Unoriented
                                    || is_valid_pair(
                                        pair,
                                        Overlap {
                                            source_in_comparison: true,
                                            comparison_in_source: true,
                                        },
                                    ))
                            {
                                expected.insert(pair);
                            }
                        }
                    }
                    assert_eq!(collect.0, expected, "{mode:?} {policy:?} {strict_prefix}");
                }
            }
            if mode != MatchMode::NamePrefix {
                let source = query(&side, "source").unwrap();
                let mut explained = join_statement(mode, &source, &source);
                let sql = format!("EXPLAIN QUERY PLAN {}", explained.sql().as_str());
                let mut query = explained.build();
                let args = sqlx::Execute::take_arguments(&mut query).unwrap().unwrap();
                let details = sqlx::query_with(sqlx::AssertSqlSafe(sql), args)
                    .fetch_all(db.read())
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|r| r.get::<String, _>("detail"))
                    .collect::<Vec<_>>()
                    .join("\n");
                let index = if mode == MatchMode::Hash {
                    "files_hash"
                } else {
                    "files_name_capture"
                };
                assert!(
                    details.contains(&format!("USING INDEX {index}")),
                    "{details}"
                );
                assert!(details.contains("SEARCH"), "{details}");
            }
        }
        cancel.cancel();
        let mut conn = db.read().acquire().await.unwrap();
        let mut collect = Collect::default();
        let p = DuplicateParams {
            mode: MatchMode::Hash,
            strict_prefix: false,
            source: side.clone(),
            comparison: side,
        };
        assert!(matches!(
            candidate_pairs(&mut conn, &p, Policy::Oriented, &cancel, &mut collect).await,
            Err(PlanError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn sink_enforces_pair_and_distinct_member_caps_before_writing() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let cancel = CancellationToken::new();
        for (max_pairs, max_matched) in [(1, 5), (5, 1)] {
            let writer = BulkWriter::new(
                &db,
                &cancel,
                "duplicate_pairs",
                &["plan_id", "source_id", "comparison_id"],
                |mut row, pair: Pair| {
                    row.push_bind(1_i64)
                        .push_bind(pair.source)
                        .push_bind(pair.comparison);
                },
            )
            .unwrap();
            let mut sink = Sink {
                writer,
                source: HashSet::new(),
                comparison: HashSet::new(),
                pairs: 0,
                max_pairs,
                max_matched,
            };
            sink.visit(Pair::new(1, 10)).await.unwrap();
            assert!(matches!(
                sink.visit(Pair::new(2, 11)).await,
                Err(PlanError::TooLarge { .. })
            ));
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM duplicate_pairs")
                .fetch_one(db.read())
                .await
                .unwrap();
            assert_eq!(count, 0);
        }
    }
}
