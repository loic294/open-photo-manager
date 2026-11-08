//! "Everything at or below a directory" as an index range, for `files.dir_path` and `dirs.path`.
//!
//! The "Subtree idiom" section of the [`crate::db`] docs explains the shape of the predicate. Use
//! [`Subtree::push_predicate`] instead of writing it by hand, and always combine it with
//! `volume_id = ?`: paths are only unique inside a volume, and the index that answers the range is
//! `(volume_id, dir_path, name)`.

use apm_core::paths::RelPath;
use sqlx::{QueryBuilder, Sqlite};

/// A directory of a volume and everything below it. The volume root is every directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subtree {
    dir: String,
}

impl Subtree {
    pub fn new(dir: &RelPath) -> Self {
        Self {
            dir: dir.as_str().to_owned(),
        }
    }

    /// The directory, as stored in `dirs.path` (`""` for the volume root).
    pub fn dir(&self) -> &str {
        &self.dir
    }

    /// The whole volume: the predicate then constrains nothing.
    pub fn is_root(&self) -> bool {
        self.dir.is_empty()
    }

    /// The in-memory twin of [`Self::push_predicate`]: is the directory `path` the subtree's
    /// directory or below it? `2019.old` is not inside `2019`.
    pub fn contains(&self, path: &str) -> bool {
        self.is_root()
            || path == self.dir
            || path
                .strip_prefix(self.dir.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    }

    /// Appends a condition that holds for the rows whose directory (`column`, for example
    /// `dir_path`, `f.dir_path` or `dirs.path`) is this directory or lies below it. The directory is
    /// bound, never spliced into the SQL; `column` must be a literal, never user input. For the root
    /// it appends `1 = 1`, so it can always follow an `AND`.
    ///
    /// The range part (`>= D AND < D || '0'`) is what the planner turns into an index search; the
    /// last two terms remove the siblings that only share the prefix (`D.old`, `D 2019`).
    pub fn push_predicate(&self, qb: &mut QueryBuilder<Sqlite>, column: &str) {
        if self.is_root() {
            qb.push("1 = 1");
            return;
        }
        qb.push("(")
            .push(column)
            .push(" >= ")
            .push_bind(self.dir.clone())
            .push(" AND ")
            .push(column)
            .push(" < ")
            .push_bind(format!("{}0", self.dir))
            .push(" AND (")
            .push(column)
            .push(" = ")
            .push_bind(self.dir.clone())
            .push(" OR ")
            .push(column)
            .push(" >= ")
            .push_bind(format!("{}/", self.dir))
            .push("))");
    }

    /// Strict descendants of a non-root directory supplied by a correlated SQL column.
    /// Both column names must be trusted literals. Combine with an equality on volume_id.
    /// Keep the exact-directory probe separate so each EXISTS uses an index SEARCH.
    pub fn push_correlated_descendants(
        qb: &mut QueryBuilder<Sqlite>,
        column: &str,
        directory: &str,
    ) {
        qb.push("(")
            .push(column)
            .push(" >= (")
            .push(directory)
            .push(" || '/') AND ")
            .push(column)
            .push(" < (")
            .push(directory)
            .push(" || '0'))");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use sqlx::Row as _;

    use super::*;
    use crate::db::Db;
    use crate::db::testing::{insert_dir, insert_file, insert_volume};

    fn subtree(dir: &str) -> Subtree {
        Subtree::new(&RelPath::new(dir).unwrap())
    }

    /// Directories that are easy to get wrong: prefix siblings, neighbours of `/` and `0` in the
    /// byte order, case and Unicode variants, astral code points.
    const DIRS: &[&str] = &[
        "",
        "2019",
        "2019/Rome",
        "2019/Rome/day 1",
        "2019.old",
        "2019.old/x",
        "2019-x",
        "2019 b",
        "2019!",
        "2019~",
        "20190",
        "20191/a",
        "2019_x",
        "2018",
        "2020",
        "201",
        "20",
        "a",
        "a/b",
        "a/b/c",
        "a/bb",
        "a/b.c",
        "a b",
        "a-b",
        "a0",
        "a1",
        "A",
        "A/b",
        "\u{e9}t\u{e9}",
        "\u{e9}t\u{e9}/p",
        "e\u{301}te\u{301}",
        "e\u{301}te\u{301}/p",
        "\u{65e5}\u{672c}/\u{5199}\u{771f}",
        "\u{65e5}\u{672c}0",
        "emoji/\u{1f600}",
        "emoji\u{1f600}",
        "z",
    ];

    async fn seeded() -> (crate::db::TempDb, i64, i64) {
        let db = Db::open_temp().await.unwrap();
        let volume = insert_volume(&db, "photos").await;
        let other = insert_volume(&db, "archive").await;
        for dir in DIRS {
            insert_dir(&db, volume, dir).await;
            insert_file(&db, volume, dir, "one.jpg").await;
            insert_file(&db, volume, dir, "two.jpg").await;
            // The same layout on another volume must never leak into the results.
            insert_dir(&db, other, dir).await;
            insert_file(&db, other, dir, "one.jpg").await;
        }
        (db, volume, other)
    }

    fn files_query(volume: i64, subtree: &Subtree, explain: bool) -> QueryBuilder<Sqlite> {
        let mut qb = QueryBuilder::new(if explain {
            "EXPLAIN QUERY PLAN SELECT id, dir_path, name FROM files WHERE volume_id = "
        } else {
            "SELECT id, dir_path, name FROM files WHERE volume_id = "
        });
        qb.push_bind(volume).push(" AND ");
        subtree.push_predicate(&mut qb, "dir_path");
        qb
    }

    fn dirs_query(volume: i64, subtree: &Subtree, explain: bool) -> QueryBuilder<Sqlite> {
        let mut qb = QueryBuilder::new(if explain {
            "EXPLAIN QUERY PLAN SELECT id, path FROM dirs WHERE volume_id = "
        } else {
            "SELECT id, path FROM dirs WHERE volume_id = "
        });
        qb.push_bind(volume).push(" AND ");
        subtree.push_predicate(&mut qb, "path");
        qb
    }

    async fn plan(db: &Db, mut qb: QueryBuilder<Sqlite>) -> String {
        // The writer connection has loaded the statistics `ANALYZE` just made; readers may not have.
        let rows = qb.build().fetch_all(db.write()).await.unwrap();
        rows.iter()
            .map(|row| row.get::<String, _>("detail"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn contains_follows_the_definition() {
        let rome = subtree("2019/Rome");
        assert!(rome.contains("2019/Rome"));
        assert!(rome.contains("2019/Rome/day 1"));
        assert!(!rome.contains("2019"));
        assert!(!rome.contains("2019/Rome2"));
        assert!(!rome.contains("2019/Rome.old"));
        assert!(!rome.contains("2019/Rom"));
        assert!(!rome.contains(""));
        let root = subtree("");
        assert!(root.is_root());
        assert!(root.contains(""));
        assert!(root.contains("anything/at/all"));
    }

    #[tokio::test]
    async fn predicate_selects_exactly_the_subtree() {
        let (db, volume, _other) = seeded().await;
        for dir in DIRS {
            let subtree = subtree(dir);
            let expected_dirs: BTreeSet<String> = DIRS
                .iter()
                .filter(|path| subtree.contains(path))
                .map(|path| (*path).to_owned())
                .collect();

            let mut qb = dirs_query(volume, &subtree, false);
            let got: BTreeSet<String> = qb
                .build()
                .fetch_all(db.read())
                .await
                .unwrap()
                .iter()
                .map(|row| row.get("path"))
                .collect();
            assert_eq!(got, expected_dirs, "dirs below {dir:?}");

            let mut qb = files_query(volume, &subtree, false);
            let rows = qb.build().fetch_all(db.read()).await.unwrap();
            let got: BTreeSet<String> = rows.iter().map(|row| row.get("dir_path")).collect();
            assert_eq!(got, expected_dirs, "file directories below {dir:?}");
            assert_eq!(rows.len(), expected_dirs.len() * 2, "files below {dir:?}");
        }
    }

    #[tokio::test]
    async fn range_is_served_by_the_directory_index_with_and_without_statistics() {
        let (db, volume, _other) = seeded().await;
        let rome = subtree("2019/Rome");
        let expected_files =
            "INDEX files_volume_dir_name (volume_id=? AND dir_path>? AND dir_path<?)";
        let expected_dirs = "INDEX dirs_volume_path (volume_id=? AND path>? AND path<?)";

        // A fresh database has no sqlite_stat1, which is when the naive form scans the volume.
        let files = plan(&db, files_query(volume, &rome, true)).await;
        assert!(
            files.contains(expected_files),
            "files, no statistics:\n{files}"
        );
        let dirs = plan(&db, dirs_query(volume, &rome, true)).await;
        assert!(dirs.contains(expected_dirs), "dirs, no statistics:\n{dirs}");

        sqlx::query("ANALYZE").execute(db.write()).await.unwrap();

        let files = plan(&db, files_query(volume, &rome, true)).await;
        assert!(files.contains(expected_files), "files, analyzed:\n{files}");
        let dirs = plan(&db, dirs_query(volume, &rome, true)).await;
        assert!(dirs.contains(expected_dirs), "dirs, analyzed:\n{dirs}");
    }

    #[tokio::test]
    async fn root_constrains_nothing_but_the_volume() {
        let (db, volume, other) = seeded().await;
        let root = subtree("");
        let mut qb = files_query(volume, &root, false);
        let rows = qb.build().fetch_all(db.read()).await.unwrap();
        assert_eq!(rows.len(), DIRS.len() * 2);
        let mut qb = files_query(other, &root, false);
        assert_eq!(
            qb.build().fetch_all(db.read()).await.unwrap().len(),
            DIRS.len()
        );
    }

    #[tokio::test]
    async fn predicate_works_with_a_table_alias() {
        let (db, volume, _other) = seeded().await;
        let mut qb =
            QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM files AS f WHERE f.volume_id = ");
        qb.push_bind(volume).push(" AND ");
        subtree("a").push_predicate(&mut qb, "f.dir_path");
        let count: i64 = qb.build_query_scalar().fetch_one(db.read()).await.unwrap();
        // a, a/b, a/b/c, a/bb, a/b.c: two files each.
        assert_eq!(count, 10);
    }
}
