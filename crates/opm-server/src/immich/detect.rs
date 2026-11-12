//! Finding the path prefix Immich uses for a volume.
//!
//! A mapping needs the name of the volume's root inside the Immich container (`/mnt/photo`), which
//! the user has to look up in Immich's compose file and may get wrong. This module suggests it.
//!
//! # How
//!
//! About [`SAMPLE_SIZE`] media files of the volume, spread evenly over the index, are looked up in
//! Immich by their exact file name (`POST /search/metadata`, one page, trashed assets included,
//! optionally restricted to one library). Every asset found has an `originalPath`; when that path
//! ends with `/<relative path of the file>`, what is in front of it is a candidate prefix:
//! `/mnt/photo/2019/IMG_1.jpg` for the file `2019/IMG_1.jpg` makes `/mnt/photo`. Each file votes
//! once for every distinct candidate it produced, and the suggestions are the candidates by number
//! of votes, at most [`MAX_SUGGESTIONS`]. A file whose name Immich does not know, or knows only in
//! other folders, simply does not vote.
//!
//! The comparison is byte for byte on whole path segments (the rule of [`PathMap`]): a different
//! Unicode form of the same name is a different path to Immich and gets no vote, and
//! `/mnt/photo/xx2019/IMG_1.jpg` is not the file `2019/IMG_1.jpg` under `/mnt/photo/xx`. A candidate
//! the mapping rules would refuse (relative, `..`, `//`) is dropped.
//!
//! # Limits
//!
//! This is a request the user waits for, not a task: at most [`CONCURRENCY`] lookups run at once
//! and all of them together stop after [`TIME_LIMIT`], which is below the limit of the HTTP layer.
//! Lookups that finished by then still produce suggestions. When none did, the first error is
//! returned. An error that every further lookup would repeat (Immich unreachable, key refused)
//! stops the run at once.
//!
//! [`PathMap`]: super::PathMap

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream;
use opm_core::dto::{
    FieldError, ImmichDetectPrefixRequest, ImmichDetectPrefixResponse, ImmichPrefixSuggestion,
};
use opm_core::paths::{RelPath, VolumeId};
use tokio::time::Instant;

use crate::db::{Db, DbError, volumes};

use super::ImmichService;
use super::client::{ImmichClient, is_safe_id};
use super::error::ImmichError;
use super::models::MetadataSearch;
use super::paths::check_prefix;

/// How many files of the volume are looked up in Immich.
const SAMPLE_SIZE: u32 = 30;
/// How many lookups run at the same time.
const CONCURRENCY: usize = 4;
/// All lookups together stop after this long.
const TIME_LIMIT: Duration = Duration::from_secs(20);
/// At most this many suggestions are returned.
const MAX_SUGGESTIONS: usize = 5;
/// Assets read for one file name: more than this many with the same name is rare, and the first
/// page is all a lookup looks at.
const LOOKUP_PAGE_SIZE: u32 = 100;

impl ImmichService {
    /// Suggests path prefixes for the volume; see the module documentation. [`ImmichError::NotConnected`]
    /// without a connection, a validation error for an unknown volume or an unusable library id.
    pub async fn detect_prefix(
        &self,
        request: ImmichDetectPrefixRequest,
    ) -> Result<ImmichDetectPrefixResponse, ImmichError> {
        let client = self.client().await?;
        let mut problems = Vec::new();
        if let Some(id) = request.library_id.as_deref()
            && !is_safe_id(id)
        {
            problems.push(FieldError::new(
                "library_id",
                "is not a valid Immich library id",
            ));
        }
        if volumes::get(&self.inner.db, request.volume_id)
            .await?
            .is_none()
        {
            problems.push(FieldError::new(
                "volume_id",
                "is not a volume of this application",
            ));
        }
        if !problems.is_empty() {
            return Err(ImmichError::Validation(problems));
        }

        let samples = sample_files(&self.inner.db, request.volume_id, SAMPLE_SIZE).await?;
        let library_id = request.library_id;
        let lookups = gather(samples, TIME_LIMIT, |rel| {
            let client = Arc::clone(&client);
            let library_id = library_id.clone();
            async move { remote_paths(&client, library_id.as_deref(), &rel).await }
        })
        .await?;
        Ok(ImmichDetectPrefixResponse {
            suggestions: suggest(&lookups),
        })
    }
}

/// What Immich said about one sampled file.
#[derive(Debug)]
struct Lookup {
    rel: RelPath,
    /// The `originalPath` of every asset Immich has under the file's name.
    remote_paths: Vec<String>,
}

/// The `originalPath` of every asset of Immich whose file name is that of `rel` (the first page).
async fn remote_paths(
    client: &ImmichClient,
    library_id: Option<&str>,
    rel: &RelPath,
) -> Result<Vec<String>, ImmichError> {
    let Some(name) = rel.file_name() else {
        return Ok(Vec::new());
    };
    let page = client
        .search_metadata(&MetadataSearch {
            library_id: library_id.map(str::to_owned),
            original_file_name: Some(name.to_owned()),
            include_trashed: true,
            size: LOOKUP_PAGE_SIZE,
            ..MetadataSearch::default()
        })
        .await?;
    Ok(page
        .items
        .into_iter()
        .map(|asset| asset.original_path)
        .collect())
}

/// Runs `fetch` for every sample, a few at a time, until all finished or `limit` is over.
///
/// The lookups that succeeded are returned. If none did, the first error is: what stopped the run
/// (the time limit counts as "Immich did not answer").
async fn gather<F, Fut>(
    samples: Vec<RelPath>,
    limit: Duration,
    fetch: F,
) -> Result<Vec<Lookup>, ImmichError>
where
    F: Fn(RelPath) -> Fut,
    Fut: Future<Output = Result<Vec<String>, ImmichError>>,
{
    let deadline = Instant::now() + limit;
    let mut lookups = Vec::new();
    let mut failure: Option<ImmichError> = None;
    let mut results = pin!(
        stream::iter(samples)
            .map(|rel| {
                let pending = fetch(rel.clone());
                async move { (rel, pending.await) }
            })
            .buffer_unordered(CONCURRENCY)
    );
    loop {
        match tokio::time::timeout_at(deadline, results.next()).await {
            Err(_) => {
                failure.get_or_insert_with(|| {
                    ImmichError::Unreachable("Immich did not answer in time".to_owned())
                });
                break;
            }
            Ok(None) => break,
            Ok(Some((rel, Ok(remote_paths)))) => lookups.push(Lookup { rel, remote_paths }),
            Ok(Some((_, Err(err)))) => {
                let repeats = err.affects_every_call();
                failure.get_or_insert(err);
                if repeats {
                    break;
                }
            }
        }
    }
    match failure {
        Some(err) if lookups.is_empty() => Err(err),
        _ => Ok(lookups),
    }
}

/// The candidate prefixes of the lookups, by number of votes (ties by prefix), at most
/// [`MAX_SUGGESTIONS`]. `sampled` is the number of lookups that were answered.
fn suggest(lookups: &[Lookup]) -> Vec<ImmichPrefixSuggestion> {
    let sampled = u32::try_from(lookups.len()).unwrap_or(u32::MAX);
    let mut votes: HashMap<String, u32> = HashMap::new();
    for lookup in lookups {
        let candidates: HashSet<String> = lookup
            .remote_paths
            .iter()
            .filter_map(|remote| prefix_of(remote, &lookup.rel))
            .collect();
        for candidate in candidates {
            *votes.entry(candidate).or_default() += 1;
        }
    }
    let mut ranked: Vec<(String, u32)> = votes.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(MAX_SUGGESTIONS);
    ranked
        .into_iter()
        .map(|(prefix, matched)| ImmichPrefixSuggestion {
            prefix,
            matched,
            sampled,
        })
        .collect()
}

/// The prefix that turns `rel` into `remote`, if `remote` is `<prefix>/<rel>` and the prefix is one
/// a mapping may use.
fn prefix_of(remote: &str, rel: &RelPath) -> Option<String> {
    if rel.is_root() {
        return None;
    }
    let head = remote.strip_suffix(rel.as_str())?.strip_suffix('/')?;
    let prefix = if head.is_empty() { "/" } else { head };
    check_prefix(prefix).ok()?;
    Some(prefix.to_owned())
}

/// Up to `count` media files of the volume, evenly spread over its index in path order, so every
/// part of the volume has a say.
async fn sample_files(db: &Db, volume: VolumeId, count: u32) -> Result<Vec<RelPath>, DbError> {
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM files WHERE volume_id = ? AND kind IN ('image', 'video')",
    )
    .bind(volume)
    .fetch_one(db.read())
    .await?;
    let total = u64::try_from(total).unwrap_or(0);
    if total == 0 || count == 0 {
        return Ok(Vec::new());
    }
    let stride = total.div_ceil(u64::from(count)).max(1);
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT dir_path, name FROM ( \
             SELECT dir_path, name, ROW_NUMBER() OVER (ORDER BY dir_path, name) AS rn \
             FROM files WHERE volume_id = ? AND kind IN ('image', 'video') \
         ) WHERE (rn - 1) % ? = 0 ORDER BY rn LIMIT ?",
    )
    .bind(volume)
    .bind(i64::try_from(stride).unwrap_or(i64::MAX))
    .bind(i64::from(count))
    .fetch_all(db.read())
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(dir, name)| RelPath::new(&dir).and_then(|dir| dir.join(&name)).ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn rel(path: &str) -> RelPath {
        RelPath::new(path).unwrap()
    }

    fn lookup(path: &str, remote: &[&str]) -> Lookup {
        Lookup {
            rel: rel(path),
            remote_paths: remote.iter().map(|path| (*path).to_owned()).collect(),
        }
    }

    fn prefixes(lookups: &[Lookup]) -> Vec<(String, u32, u32)> {
        suggest(lookups)
            .into_iter()
            .map(|s| (s.prefix, s.matched, s.sampled))
            .collect()
    }

    #[test]
    fn the_prefix_with_most_votes_comes_first_and_ties_go_by_prefix() {
        let lookups = [
            lookup(
                "2019/a.jpg",
                &["/mnt/photo/2019/a.jpg", "/mnt/backup/2019/a.jpg"],
            ),
            lookup("2019/b.jpg", &["/mnt/photo/2019/b.jpg"]),
            lookup("2020/c.jpg", &["/mnt/backup/2020/c.jpg"]),
            lookup("2020/d.jpg", &["/elsewhere/2020/other.jpg"]),
        ];
        assert_eq!(
            prefixes(&lookups),
            [
                ("/mnt/backup".to_owned(), 2, 4),
                ("/mnt/photo".to_owned(), 2, 4),
            ]
        );
    }

    #[test]
    fn at_most_five_suggestions_are_returned() {
        let lookups: Vec<Lookup> = (0..8)
            .map(|i| {
                let remote = format!("/mnt/lib{i}/a.jpg");
                lookup("a.jpg", &[remote.as_str()])
            })
            .collect();
        let suggestions = suggest(&lookups);
        assert_eq!(suggestions.len(), MAX_SUGGESTIONS);
        assert!(suggestions.iter().all(|s| s.sampled == 8));
    }

    #[test]
    fn a_file_votes_once_for_a_prefix_however_many_assets_show_it() {
        let lookups = [lookup(
            "a/b.jpg",
            &[
                "/mnt/photo/a/b.jpg",
                "/mnt/photo/a/b.jpg",
                "/mnt/photo/a/b.jpg",
            ],
        )];
        assert_eq!(prefixes(&lookups), [("/mnt/photo".to_owned(), 1, 1)]);
    }

    #[test]
    fn only_whole_path_segments_match() {
        let lookups = [lookup(
            "2019/IMG_1.jpg",
            &[
                "/mnt/photo/xx2019/IMG_1.jpg",
                "/mnt/photo/2019/IMG_1.jpg.bak",
                "/mnt/photo/2019/IMG_1.jpg/",
            ],
        )];
        assert!(suggest(&lookups).is_empty());
    }

    #[test]
    fn a_file_at_the_root_of_the_volume_can_vote_for_the_root_prefix() {
        let lookups = [lookup("IMG_1.jpg", &["/IMG_1.jpg", "/mnt/photo/IMG_1.jpg"])];
        assert_eq!(
            prefixes(&lookups),
            [("/".to_owned(), 1, 1), ("/mnt/photo".to_owned(), 1, 1)]
        );
    }

    #[test]
    fn prefixes_the_mapping_rules_refuse_are_dropped() {
        let lookups = [lookup(
            "2019/IMG_1.jpg",
            &[
                "relative/2019/IMG_1.jpg",
                "/mnt//photo/2019/IMG_1.jpg",
                "/mnt/../photo/2019/IMG_1.jpg",
                "/mnt/./photo/2019/IMG_1.jpg",
                "/mnt/photo/2019/IMG_1.jpg",
            ],
        )];
        assert_eq!(prefixes(&lookups), [("/mnt/photo".to_owned(), 1, 1)]);
    }

    #[test]
    fn a_different_unicode_form_of_the_name_is_a_different_path() {
        let decomposed = "caf\u{65}\u{301}/photo.jpg";
        let composed = "/mnt/photo/caf\u{e9}/photo.jpg";
        let lookups = [lookup(decomposed, &[composed])];
        assert!(suggest(&lookups).is_empty());

        let same_form = "/mnt/photo/caf\u{65}\u{301}/photo.jpg";
        let lookups = [lookup(decomposed, &[same_form])];
        assert_eq!(prefixes(&lookups), [("/mnt/photo".to_owned(), 1, 1)]);
    }

    #[test]
    fn the_root_has_no_file_name_to_match() {
        let root = Lookup {
            rel: RelPath::root(),
            remote_paths: vec!["/mnt/photo".to_owned()],
        };
        assert!(suggest(&[root]).is_empty());
    }

    #[tokio::test]
    async fn at_most_four_lookups_run_at_the_same_time() {
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let samples: Vec<RelPath> = (0..20).map(|i| rel(&format!("d/IMG_{i}.jpg"))).collect();

        let lookups = gather(samples, Duration::from_secs(10), |rel| {
            let running = Arc::clone(&running);
            let peak = Arc::clone(&peak);
            async move {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await;
                running.fetch_sub(1, Ordering::SeqCst);
                Ok(vec![format!("/mnt/photo/{rel}")])
            }
        })
        .await
        .unwrap();

        assert_eq!(lookups.len(), 20);
        let peak = peak.load(Ordering::SeqCst);
        assert!(peak <= CONCURRENCY, "{peak} lookups ran at once");
        assert!(peak > 1, "the lookups should overlap");
    }

    #[tokio::test]
    async fn failed_lookups_are_not_counted_as_sampled() {
        let samples = vec![rel("a/1.jpg"), rel("a/2.jpg"), rel("a/3.jpg")];
        let lookups = gather(samples, Duration::from_secs(10), |rel| async move {
            if rel.as_str() == "a/2.jpg" {
                Err(ImmichError::Upstream {
                    status: 500,
                    message: "boom".to_owned(),
                })
            } else {
                Ok(vec![format!("/mnt/photo/{rel}")])
            }
        })
        .await
        .unwrap();
        assert_eq!(prefixes(&lookups), [("/mnt/photo".to_owned(), 2, 2)]);
    }

    #[tokio::test]
    async fn when_every_lookup_fails_the_first_error_is_returned() {
        let samples = vec![rel("a/1.jpg"), rel("a/2.jpg")];
        let err = gather(samples, Duration::from_secs(10), |_| async {
            Err::<Vec<String>, _>(ImmichError::Upstream {
                status: 500,
                message: "boom".to_owned(),
            })
        })
        .await
        .unwrap_err();
        assert!(
            matches!(err, ImmichError::Upstream { status: 500, .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn an_error_that_would_repeat_stops_the_run() {
        let calls = Arc::new(AtomicUsize::new(0));
        let samples: Vec<RelPath> = (0..30).map(|i| rel(&format!("d/{i}.jpg"))).collect();
        let err = gather(samples, Duration::from_secs(10), |_| {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(5)).await;
                Err::<Vec<String>, _>(ImmichError::Unreachable("down".to_owned()))
            }
        })
        .await
        .unwrap_err();
        assert!(matches!(err, ImmichError::Unreachable(_)), "{err:?}");
        assert!(
            calls.load(Ordering::SeqCst) <= 2 * CONCURRENCY,
            "{} lookups were started",
            calls.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn the_time_limit_keeps_what_was_found() {
        let samples = vec![rel("a/1.jpg"), rel("a/2.jpg"), rel("a/3.jpg")];
        let started = std::time::Instant::now();
        let lookups = gather(samples, Duration::from_millis(150), |rel| async move {
            if rel.as_str() == "a/3.jpg" {
                std::future::pending().await
            } else {
                Ok(vec![format!("/mnt/photo/{rel}")])
            }
        })
        .await
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(prefixes(&lookups), [("/mnt/photo".to_owned(), 2, 2)]);
    }

    #[tokio::test]
    async fn when_nothing_answers_in_time_the_run_ends_with_an_error() {
        let samples = vec![rel("a/1.jpg"), rel("a/2.jpg")];
        let started = std::time::Instant::now();
        let err = gather(samples, Duration::from_millis(100), |_| async {
            std::future::pending::<Result<Vec<String>, ImmichError>>().await
        })
        .await
        .unwrap_err();
        assert!(matches!(err, ImmichError::Unreachable(_)), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn no_samples_means_no_suggestions_and_no_lookups() {
        let lookups = gather(Vec::new(), Duration::from_secs(1), |_| async {
            panic!("nothing to look up")
        })
        .await
        .unwrap_or_else(|_: ImmichError| panic!("an empty sample cannot fail"));
        assert!(suggest(&lookups).is_empty());
    }

    async fn add_volume(db: &Db, name: &str) -> VolumeId {
        sqlx::query_scalar(
            "INSERT INTO volumes (name, root, writable, online, created_at) \
             VALUES (?, ?, 1, 1, 0) RETURNING id",
        )
        .bind(name)
        .bind(format!("/volumes/{name}"))
        .fetch_one(db.write())
        .await
        .unwrap()
    }

    async fn add_file(db: &Db, volume: VolumeId, dir: &str, name: &str, kind: &str) {
        sqlx::query(
            "INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, kind, size, mtime) \
             VALUES (?, ?, ?, ?, ?, ?, 1, 1)",
        )
        .bind(volume)
        .bind(dir)
        .bind(name)
        .bind(name.to_lowercase())
        .bind(name.to_lowercase())
        .bind(kind)
        .execute(db.write())
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn the_sample_is_spread_over_the_media_files_of_one_volume() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let volume = add_volume(&db, "photo").await;
        let other = add_volume(&db, "other").await;
        for i in 0..100 {
            add_file(
                &db,
                volume,
                &format!("d{:02}", i / 10),
                &format!("{i:03}.jpg"),
                "image",
            )
            .await;
        }
        add_file(&db, volume, "d00", "notes.txt", "other").await;
        add_file(&db, volume, "d00", "a.xmp", "sidecar").await;
        add_file(&db, other, "x", "elsewhere.jpg", "image").await;

        let sample = sample_files(&db, volume, 10).await.unwrap();
        assert_eq!(sample.len(), 10);
        let names: Vec<&str> = sample.iter().map(|rel| rel.as_str()).collect();
        assert_eq!(names.first(), Some(&"d00/000.jpg"));
        assert_eq!(names.last(), Some(&"d09/090.jpg"));
        assert!(
            names
                .iter()
                .all(|name| name.ends_with(".jpg") && !name.contains("elsewhere"))
        );
        let directories: HashSet<&str> = names.iter().filter_map(|n| n.split('/').next()).collect();
        assert_eq!(directories.len(), 10, "every directory has a say");

        let few = sample_files(&db, other, 30).await.unwrap();
        assert_eq!(few.len(), 1);
        assert_eq!(few[0].as_str(), "x/elsewhere.jpg");
    }

    #[tokio::test]
    async fn files_at_the_root_and_an_empty_volume_are_handled() {
        let temp = Db::open_temp().await.unwrap();
        let db = temp.handle();
        let volume = add_volume(&db, "photo").await;
        assert!(sample_files(&db, volume, 30).await.unwrap().is_empty());

        add_file(&db, volume, "", "top.jpg", "image").await;
        add_file(&db, volume, "clips", "a.mp4", "video").await;
        let sample = sample_files(&db, volume, 30).await.unwrap();
        let names: Vec<&str> = sample.iter().map(|rel| rel.as_str()).collect();
        assert_eq!(names, ["top.jpg", "clips/a.mp4"]);
    }
}
