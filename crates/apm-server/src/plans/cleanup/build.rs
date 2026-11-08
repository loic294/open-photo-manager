use apm_core::dto::CleanupParams;
use apm_core::junk::JunkRules;
use apm_core::paths::RelPath;
use futures_util::TryStreamExt;
use serde_json::Value;
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::db::subtree::Subtree;
use crate::fsops::is_protected_path;
use crate::plans::{BuildContext, Built, BulkWriter, PlanError};

pub const MAX_FOLDERS: u64 = 1_000_000;

fn check_folder_count(count: u64) -> Result<(), PlanError> {
    if count > MAX_FOLDERS {
        tracing::warn!("cleanup folder limit exceeded; pick a deeper base folder");
        Err(PlanError::TooLarge {
            items: count,
            max: MAX_FOLDERS,
        })
    } else {
        Ok(())
    }
}

fn push_empty(qb: &mut QueryBuilder<Sqlite>, directory: &str) {
    qb.push("(NOT EXISTS (SELECT 1 FROM files f WHERE f.volume_id = d.volume_id AND f.dir_path = ")
        .push(directory)
        .push(") AND NOT EXISTS (SELECT 1 FROM files f WHERE f.volume_id = d.volume_id AND ");
    Subtree::push_correlated_descendants(qb, "f.dir_path", directory);
    qb.push("))");
}

pub(super) fn query(volume: i64, base: &RelPath, recursive: bool) -> QueryBuilder<Sqlite> {
    let mut qb = QueryBuilder::new("WITH scoped AS (SELECT d.volume_id, d.path, d.parent_path, ");
    if !base.is_root() {
        qb.push_bind(format!("{}/", base.as_str())).push(" || ");
    }
    crate::query::push_child_segment(&mut qb, "d.path", base.as_str());
    qb.push(" AS branch FROM dirs d WHERE d.volume_id = ")
        .push_bind(volume)
        .push(" AND ");
    Subtree::new(base).push_predicate(&mut qb, "d.path");
    qb.push(" AND d.path <> ")
        .push_bind(base.as_str().to_owned());
    qb.push(") SELECT d.path, d.parent_path, ");
    push_empty(&mut qb, "d.path");
    qb.push(" AS empty, ");
    if recursive {
        qb.push("1");
    } else {
        push_empty(&mut qb, "d.branch");
    }
    qb.push(" AS branch_empty FROM scoped d");
    qb
}

pub(super) async fn build(ctx: &BuildContext, params: Value) -> Result<Built, PlanError> {
    ctx.check_cancelled()?;
    let params: CleanupParams = serde_json::from_value(params)
        .map_err(|err| PlanError::invalid_field("scope", err.to_string()))?;
    let volume_id = params
        .scope
        .volume_id
        .ok_or_else(|| PlanError::invalid_field("scope", "Open a volume folder first"))?;
    let volume = ctx
        .volumes
        .volume(volume_id)
        .ok_or_else(|| PlanError::invalid_field("scope", "Unknown volume"))?;
    if !volume.online || !volume.writable {
        return Err(PlanError::invalid_field(
            "scope",
            "The volume must be online and writable",
        ));
    }
    let base = RelPath::new(&params.scope.dir)
        .map_err(|err| PlanError::invalid_field("scope", err.to_string()))?;
    if base.as_str() != params.scope.dir {
        return Err(PlanError::invalid_field(
            "scope",
            "Use the exact indexed folder path",
        ));
    }
    let junk = JunkRules::new(&ctx.settings.junk_names, &ctx.settings.junk_prefixes);
    if is_protected_path(&base) || base.components().any(|name| junk.is_junk_dir(name)) {
        return Err(PlanError::invalid_field(
            "scope",
            "The base cannot be an ignored or system folder",
        ));
    }
    let mut snapshot = ctx.db.read().begin().await?;
    if !base.is_root()
        && !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM dirs WHERE volume_id = ? AND path = ?)",
        )
        .bind(volume_id)
        .bind(base.as_str())
        .fetch_one(&mut *snapshot)
        .await?
    {
        return Err(PlanError::invalid_field(
            "scope",
            "The base folder is not in the index",
        ));
    }
    let indexing: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM tasks t WHERE t.kind = 'index' \
         AND t.state IN ('queued', 'running') AND \
         (json_array_length(t.params_json, '$.targets') IS NULL \
          OR json_array_length(t.params_json, '$.targets') = 0 \
          OR EXISTS (SELECT 1 FROM json_each(t.params_json, '$.targets') j \
                     WHERE json_extract(j.value, '$.volume_id') = ? \
                        OR json_extract(j.value, '$.volume_id') IS NULL)))",
    )
    .bind(volume_id)
    .fetch_one(&mut *snapshot)
    .await?;
    let mut qb = query(volume_id, &base, params.scope.recursive);
    let mut rows = qb.build().fetch(&mut *snapshot);
    let plan_id = ctx.plan_id;
    let mut writer = BulkWriter::new(
        &ctx.db,
        &ctx.cancel,
        "cleanup_dirs",
        &["plan_id", "path", "depth"],
        move |mut row, (path, depth): (String, i64)| {
            row.push_bind(plan_id).push_bind(path).push_bind(depth);
        },
    )?;
    let mut scanned = 0;
    let mut empty = 0;
    let mut planned = 0;
    while let Some(row) = rows.try_next().await? {
        ctx.check_cancelled()?;
        scanned += 1;
        check_folder_count(scanned)?;
        if !row.get::<bool, _>("empty") {
            continue;
        }
        empty += 1;
        if !row.get::<bool, _>("branch_empty") {
            continue;
        }
        let path: String = row.get("path");
        let rel = RelPath::new(&path).map_err(|err| PlanError::Internal(err.into()))?;
        // Rows in ignored directories must never become removal targets, even in a stale index.
        if is_protected_path(&rel) || rel.components().any(|name| junk.is_junk_dir(name)) {
            continue;
        }
        planned += 1;
        ctx.check_size(planned)?;
        writer.push((path, rel.depth() as i64)).await?;
    }

    drop(rows);
    snapshot.commit().await?;
    let items = writer.finish().await?;
    ctx.check_size(items)?;
    ctx.check_cancelled()?;
    let top = top_level_count(ctx).await?;
    Ok(Built::new(items)
        .stat("folders_scanned", scanned)
        .stat("empty_folders", items)
        .stat("top_level_empty_folders", top)
        .stat("nonempty_folders", scanned - empty)
        .warning("index_running",
            "Indexing is still running or queued. Every folder will be checked again before deletion.",
            u64::from(indexing)))
}

async fn top_level_count(ctx: &BuildContext) -> Result<u64, PlanError> {
    let mut after = String::new();
    let mut count = 0;
    loop {
        ctx.check_cancelled()?;
        let paths: Vec<String> = sqlx::query_scalar(
            "SELECT path FROM cleanup_dirs WHERE plan_id = ? AND path > ? ORDER BY path LIMIT 1000",
        )
        .bind(ctx.plan_id)
        .bind(&after)
        .fetch_all(ctx.db.read())
        .await?;
        if paths.is_empty() {
            break;
        }
        let mut parents = Vec::with_capacity(paths.len());
        for path in &paths {
            let rel = RelPath::new(path).map_err(|err| PlanError::Internal(err.into()))?;
            parents.push(rel.parent().unwrap_or_default().as_str().to_owned());
        }
        let mut present = std::collections::HashSet::new();
        for chunk in parents.chunks(500) {
            let mut qb =
                QueryBuilder::<Sqlite>::new("SELECT path FROM cleanup_dirs WHERE plan_id = ");
            qb.push_bind(ctx.plan_id).push(" AND path IN (");
            let mut separated = qb.separated(", ");
            for parent in chunk {
                separated.push_bind(parent.clone());
            }
            separated.push_unseparated(")");
            present.extend(
                qb.build_query_scalar::<String>()
                    .fetch_all(ctx.db.read())
                    .await?,
            );
        }
        count += parents
            .iter()
            .filter(|path| !present.contains(*path))
            .count() as u64;
        if let Some(last) = paths.last() {
            after.clone_from(last);
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use proptest::prelude::*;
    use sqlx::Execute;

    use super::*;
    use crate::db::{
        Db,
        testing::{insert_dir, insert_file, insert_volume},
    };

    #[test]
    fn folder_limit_counts_examined_not_only_empty_folders() {
        assert!(check_folder_count(MAX_FOLDERS).is_ok());
        assert!(matches!(
            check_folder_count(MAX_FOLDERS + 1),
            Err(PlanError::TooLarge {
                items: 1_000_001,
                max: 1_000_000
            })
        ));
    }

    #[tokio::test]
    async fn correlated_file_probes_search_the_binary_index_and_keep_unicode_case_distinct() {
        let db = Db::open_temp().await.unwrap();
        let v = insert_volume(&db, "photos").await;
        for dir in [
            "base",
            "base/A",
            "base/a",
            "base/A_b",
            "base/A (copy)",
            "base/A!x",
            "base/A/child",
            "base/été",
            "base/e\u{301}te\u{301}",
            "base/日本",
        ] {
            insert_dir(&db, v, dir).await;
        }
        insert_file(&db, v, "base/A_b", "data.txt").await;
        insert_file(&db, v, "base/a", "edits.xmp").await;
        let base = RelPath::new("base").unwrap();
        let mut qb = query(v, &base, true);
        let mut statement = qb.build();
        let arguments = statement.take_arguments().unwrap().unwrap_or_default();
        let sql = format!("EXPLAIN QUERY PLAN {}", statement.sql().as_str());
        let explain = sqlx::query_with(sqlx::AssertSqlSafe(sql), arguments)
            .fetch_all(db.read())
            .await
            .unwrap();
        let detail = explain
            .iter()
            .map(|row| row.get::<String, _>("detail"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            detail.contains(
                "SEARCH f USING COVERING INDEX files_volume_dir_name (volume_id=? AND dir_path=?)"
            ),
            "{detail}"
        );
        assert!(detail.contains("SEARCH f USING COVERING INDEX files_volume_dir_name (volume_id=? AND dir_path>? AND dir_path<?)"), "{detail}");
        assert!(!detail.contains("SCAN f"), "{detail}");
        let rows = query(v, &base, true)
            .build()
            .fetch_all(db.read())
            .await
            .unwrap();
        let empty: BTreeSet<String> = rows
            .iter()
            .filter(|row| row.get::<bool, _>("empty"))
            .map(|row| row.get("path"))
            .collect();
        assert!(empty.contains("base/A"));
        assert!(!empty.contains("base/a"));
        assert!(empty.contains("base/été"));
        assert!(empty.contains("base/e\u{301}te\u{301}"));
        assert!(empty.contains("base/日本"));
        // Unicode base lengths use SQLite's character offsets in direct-child mode.
        insert_dir(&db, v, "base/été/child").await;
        let rows = query(v, &RelPath::new("base/été").unwrap(), false)
            .build()
            .fetch_all(db.read())
            .await
            .unwrap();
        assert!(rows[0].get::<bool, _>("branch_empty"));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]
        #[test]
        fn random_trees_match_the_naive_oracle(
            nodes in prop::collection::vec((0usize..40, any::<bool>()), 1..80),
            recursive in any::<bool>()
        ) {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let db = Db::open_temp().await.unwrap();
                let v = insert_volume(&db, "photos").await;
                insert_dir(&db, v, "base").await;
                let mut dirs = vec!["base".to_owned()];
                let mut content = BTreeSet::new();
                for (i, (parent, has_content)) in nodes.iter().enumerate() {
                    let parent = &dirs[parent % dirs.len()];
                    let path = format!("{parent}/d{i}");
                    insert_dir(&db, v, &path).await;
                    if *has_content {
                        insert_file(&db, v, &path, "orphan.xmp").await;
                        content.insert(path.clone());
                    }
                    dirs.push(path);
                }
                let is_empty = |path: &str| !content.iter().any(|f|
                    Subtree::new(&RelPath::new(path).unwrap()).contains(f));
                let oracle: BTreeSet<String> = dirs.iter().skip(1).filter(|path| {
                    if !is_empty(path) { return false; }
                    if recursive { return true; }
                    let branch = path.strip_prefix("base/").unwrap().split('/').next().unwrap();
                    is_empty(&format!("base/{branch}"))
                }).cloned().collect();
                let rows = query(v, &RelPath::new("base").unwrap(), recursive)
                    .build().fetch_all(db.read()).await.unwrap();
                let actual: BTreeSet<String> = rows.iter().filter(|row|
                    row.get::<bool,_>("empty") && row.get::<bool,_>("branch_empty"))
                    .map(|row| row.get("path")).collect();
                assert_eq!(actual, oracle);
            });
        }
    }
}
