//! The exiftool implementation of [`MetadataExtractor`]: a small pool of `-stay_open` processes.
//!
//! # The command
//! `exiftool -stay_open True -@ -` is started once per worker (through `security::proc`, so with a
//! cleared environment, its own process group and the private `HOME`). Each batch of up to
//! [`BATCH_FILES`] files is written to its stdin as an argument file: the options of
//! [`base_args`], one absolute path per line, `-execute<n>`; the answer is a JSON array on stdout
//! followed by the line `{ready<n>}`. The options are read-only (`-j -G1 -n -fast`, a
//! `-charset filename=utf8` and one `-Tag` per tag read, see [`TAGS`]); nothing that writes, runs
//! code or reads another file (`-w`, `-tagsfromfile`, `-if`, `-p`, `-@`...) is ever built, and the
//! builder is tested for that.
//!
//! A path that contains a line break, or starts or ends with whitespace, cannot be written safely
//! as a line of an argument file (it could inject options); those files take a one-shot
//! `exiftool` run with the path as a single argument.
//!
//! # Failure handling
//! A batch has [`BATCH_TIMEOUT`]. When it times out, the process dies or its answer cannot be
//! read, the worker is killed and dropped (a new one starts on demand) and every file of the batch
//! is retried alone with [`SINGLE_TIMEOUT`], so one poisonous file costs one `Timeout` / `Crashed`
//! outcome, not the batch. A file missing from a successful answer, or with an `Error` tag, is
//! [`ExtractFailure::Unreadable`]; one with only a `SourceFile` simply has no metadata.
//!
//! The minimum supported exiftool is 12.24 ([`MIN_VERSION`]); [`ExiftoolExtractor::probe`] checks
//! it. The Docker image needs `exiftool` with `perl`, no extra Perl modules.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use opm_core::dto::FileKind;
use opm_core::metadata::{SourceMetadata, flatten_keywords};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::Semaphore;

use super::{Embedded, ExtractFailure, ExtractOutcome, ExtractRequest, MetadataExtractor};
use crate::security::proc::{self, ChildGuard, ProcSpec};

/// Files per exiftool batch.
pub const BATCH_FILES: usize = 32;
/// Time allowed for one batch.
pub const BATCH_TIMEOUT: Duration = Duration::from_secs(60);
/// Time allowed for one file retried alone.
pub const SINGLE_TIMEOUT: Duration = Duration::from_secs(20);
/// The oldest exiftool whose output this module is written for.
pub const MIN_VERSION: f64 = 12.24;
const MAX_OUTPUT: usize = 32 << 20;

/// The tags requested, in exiftool's names (any group).
pub const TAGS: &[&str] = &[
    "DateTimeOriginal",
    "SubSecDateTimeOriginal",
    "OffsetTimeOriginal",
    "CreateDate",
    "MediaCreateDate",
    "CreationDate",
    "Make",
    "Model",
    "Rating",
    "Pick",
    "Orientation",
    "ImageWidth",
    "ImageHeight",
    "Rotation",
    "Duration",
    "MIMEType",
    "Subject",
    "Keywords",
    "HierarchicalSubject",
    "Error",
    "Warning",
];

/// The options of every command, in order; the files follow.
pub fn base_args() -> Vec<String> {
    let mut args: Vec<String> = ["-j", "-G1", "-n", "-fast", "-charset", "filename=utf8"]
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect();
    args.extend(TAGS.iter().map(|tag| format!("-{tag}")));
    args
}

/// Can `path` be a line of an argument file without changing what exiftool is asked to do?
fn safe_as_argument_line(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    text.starts_with('/') && !text.contains(['\n', '\r', '\0']) && text.trim() == text
}

struct Worker {
    guard: ChildGuard,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    sequence: u64,
}

impl Worker {
    fn start(spec: &ProcSpec) -> Result<Self, String> {
        let mut guard = ChildGuard::spawn(spec).map_err(|err| err.to_string())?;
        let stdin = guard.take_stdin().ok_or("no stdin")?;
        let stdout = guard.take_stdout().ok_or("no stdout")?;
        Ok(Self {
            guard,
            stdin,
            stdout: BufReader::new(stdout),
            sequence: 0,
        })
    }

    /// One command over `paths`; the stdout bytes before the ready marker.
    async fn run(&mut self, paths: &[&Path]) -> Result<Vec<u8>, String> {
        self.sequence += 1;
        let marker = format!("{{ready{}}}", self.sequence);
        let mut command = String::new();
        for arg in base_args() {
            command.push_str(&arg);
            command.push('\n');
        }
        for path in paths {
            command.push_str(path.to_str().ok_or("path is not UTF-8")?);
            command.push('\n');
        }
        command.push_str(&format!("-execute{}\n", self.sequence));
        self.stdin
            .write_all(command.as_bytes())
            .await
            .map_err(|err| format!("write failed: {err}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|err| format!("write failed: {err}"))?;

        let mut output = Vec::new();
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = self
                .stdout
                .read_until(b'\n', &mut line)
                .await
                .map_err(|err| format!("read failed: {err}"))?;
            if read == 0 {
                let status = self.guard.try_wait().ok().flatten();
                return Err(format!(
                    "exiftool exited ({status:?}): {}",
                    self.guard.stderr_tail()
                ));
            }
            if line.trim_ascii_end() == marker.as_bytes() {
                return Ok(output);
            }
            output.extend_from_slice(&line);
            if output.len() > MAX_OUTPUT {
                return Err("exiftool answered with too much output".to_owned());
            }
        }
    }
}

/// How a batch failed as a whole.
#[derive(Debug)]
enum BatchError {
    Timeout,
    Crashed(String),
}

/// Reads metadata with a pool of exiftool processes.
pub struct ExiftoolExtractor {
    program: OsString,
    home: PathBuf,
    batch_timeout: Duration,
    single_timeout: Duration,
    idle: Mutex<Vec<Worker>>,
    permits: Arc<Semaphore>,
    concurrency: usize,
}

impl ExiftoolExtractor {
    /// `program` is `exiftool` or a path; `home` is the private `HOME` of the processes.
    pub fn new(program: impl Into<OsString>, home: impl Into<PathBuf>, concurrency: usize) -> Self {
        let concurrency = concurrency.max(1);
        Self {
            program: program.into(),
            home: home.into(),
            batch_timeout: BATCH_TIMEOUT,
            single_timeout: SINGLE_TIMEOUT,
            idle: Mutex::new(Vec::new()),
            permits: Arc::new(Semaphore::new(concurrency)),
            concurrency,
        }
    }

    /// Shorter timeouts, for tests with a stub program.
    #[must_use]
    pub fn with_timeouts(mut self, batch: Duration, single: Duration) -> Self {
        self.batch_timeout = batch;
        self.single_timeout = single;
        self
    }

    fn spec(&self) -> ProcSpec {
        let _ = std::fs::create_dir_all(&self.home);
        ProcSpec::new(self.program.clone())
            .args(["-stay_open", "True", "-@", "-"])
            .home(self.home.clone())
            .inherit_env("TZ")
    }

    fn take_worker(&self) -> Result<Worker, String> {
        let idle = self
            .idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        match idle {
            Some(worker) => Ok(worker),
            None => Worker::start(&self.spec()),
        }
    }

    fn give_back(&self, worker: Worker) {
        let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
        if idle.len() < self.concurrency {
            idle.push(worker);
        }
    }

    async fn run_batch(
        &self,
        paths: &[&Path],
        timeout: Duration,
    ) -> Result<HashMap<String, Value>, BatchError> {
        let mut worker = self.take_worker().map_err(BatchError::Crashed)?;
        let answer = tokio::time::timeout(timeout, worker.run(paths)).await;
        let bytes = match answer {
            Err(_) => return Err(BatchError::Timeout),
            Ok(Err(message)) => return Err(BatchError::Crashed(message)),
            Ok(Ok(bytes)) => bytes,
        };
        let entries = parse_output(&bytes).map_err(BatchError::Crashed)?;
        self.give_back(worker);
        Ok(entries)
    }

    async fn one_shot(&self, path: &Path) -> ExtractOutcome {
        let spec = ProcSpec::new(self.program.clone())
            .args(base_args())
            .path_arg(path)
            .home(self.home.clone())
            .inherit_env("TZ")
            .timeout(self.single_timeout);
        match proc::run(spec).await {
            Ok(output) => match parse_output(&output.stdout) {
                Ok(entries) => entries.into_values().next().map_or_else(
                    || Err(ExtractFailure::Unreadable("exiftool gave no result".into())),
                    |entry| embedded_from_entry(&entry),
                ),
                Err(message) => Err(ExtractFailure::Crashed(message)),
            },
            Err(err) if err.is_timeout() => Err(ExtractFailure::Timeout),
            Err(err) => Err(ExtractFailure::Crashed(err.to_string())),
        }
    }

    async fn extract_chunk(&self, requests: &[ExtractRequest]) -> Vec<ExtractOutcome> {
        let Ok(_permit) = self.permits.acquire().await else {
            return failed_all(requests, ExtractFailure::Crashed("shutting down".into()));
        };
        let mut outcomes: Vec<Option<ExtractOutcome>> = vec![None; requests.len()];
        let mut batched = Vec::new();
        for (index, request) in requests.iter().enumerate() {
            if safe_as_argument_line(&request.path) {
                batched.push(index);
            } else {
                outcomes[index] = Some(self.one_shot(&request.path).await);
            }
        }
        if !batched.is_empty() {
            let paths: Vec<&Path> = batched
                .iter()
                .map(|&i| requests[i].path.as_path())
                .collect();
            match self.run_batch(&paths, self.batch_timeout).await {
                Ok(entries) => {
                    for &index in &batched {
                        let request = &requests[index];
                        outcomes[index] = Some(outcome_for(&request.path, &entries));
                    }
                }
                Err(batch_error) => {
                    tracing::warn!(?batch_error, files = batched.len(), "exiftool batch failed");
                    for &index in &batched {
                        outcomes[index] = Some(self.retry_alone(&requests[index].path).await);
                    }
                }
            }
        }
        requests
            .iter()
            .zip(outcomes)
            .map(|(request, outcome)| {
                let outcome =
                    outcome.unwrap_or(Err(ExtractFailure::Unreadable("no result".into())));
                outcome.map(|mut embedded| {
                    embedded.finish(request.kind);
                    embedded
                })
            })
            .collect()
    }

    async fn retry_alone(&self, path: &Path) -> ExtractOutcome {
        match self.run_batch(&[path], self.single_timeout).await {
            Ok(entries) => outcome_for(path, &entries),
            Err(BatchError::Timeout) => Err(ExtractFailure::Timeout),
            Err(BatchError::Crashed(message)) => Err(ExtractFailure::Crashed(message)),
        }
    }
}

fn failed_all(requests: &[ExtractRequest], failure: ExtractFailure) -> Vec<ExtractOutcome> {
    requests.iter().map(|_| Err(failure.clone())).collect()
}

impl MetadataExtractor for ExiftoolExtractor {
    fn probe(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let spec = ProcSpec::new(self.program.clone())
                .arg("-ver")
                .home(self.home.clone())
                .timeout(Duration::from_secs(20));
            let output = proc::run(spec).await.map_err(|err| {
                if err.is_not_found() {
                    "exiftool is not installed".to_owned()
                } else {
                    err.to_string()
                }
            })?;
            let text = String::from_utf8_lossy(&output.stdout);
            let version: f64 = text
                .trim()
                .parse()
                .map_err(|_| format!("unrecognised exiftool version {:?}", text.trim()))?;
            if version < MIN_VERSION {
                return Err(format!(
                    "exiftool {version} is older than the supported {MIN_VERSION}"
                ));
            }
            Ok(())
        })
    }

    fn extract(&self, requests: Vec<ExtractRequest>) -> BoxFuture<'_, Vec<ExtractOutcome>> {
        Box::pin(async move {
            let chunks = requests.chunks(BATCH_FILES);
            let results =
                futures_util::future::join_all(chunks.map(|chunk| self.extract_chunk(chunk))).await;
            results.into_iter().flatten().collect()
        })
    }
}

/// Entries of the JSON array by `SourceFile`. An empty answer is no entry.
fn parse_output(bytes: &[u8]) -> Result<HashMap<String, Value>, String> {
    if bytes.trim_ascii().is_empty() {
        return Ok(HashMap::new());
    }
    let parsed: Vec<Value> =
        serde_json::from_slice(bytes).map_err(|err| format!("unreadable answer: {err}"))?;
    Ok(parsed
        .into_iter()
        .filter_map(|entry| {
            let source = entry.get("SourceFile")?.as_str()?.to_owned();
            Some((source, entry))
        })
        .collect())
}

/// The outcome of `path` from the entries of its answer.
fn outcome_for(path: &Path, entries: &HashMap<String, Value>) -> ExtractOutcome {
    let entry = entries
        .get(path.to_string_lossy().as_ref())
        .ok_or_else(|| ExtractFailure::Unreadable("exiftool gave no result".into()))?;
    embedded_from_entry(entry)
}

/// Turns one exiftool JSON entry into metadata; see the module docs for the tag rules.
pub(super) fn embedded_from_entry(entry: &Value) -> ExtractOutcome {
    let Some(object) = entry.as_object() else {
        return Err(ExtractFailure::Unreadable("unexpected answer".into()));
    };
    let mut fields = Fields::default();
    for (key, value) in object {
        let (group, tag) = key.rsplit_once(':').unwrap_or(("", key));
        if tag == "Error" {
            return Err(ExtractFailure::Unreadable(
                text_of(value).unwrap_or_default(),
            ));
        }
        fields.add(group, tag, value);
    }
    Ok(fields.into_embedded())
}

fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(if *flag { "1" } else { "0" }.to_owned()),
        _ => None,
    }
}

fn texts_of(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items.iter().filter_map(text_of).collect(),
        other => text_of(other).into_iter().collect(),
    }
}

fn number_of(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

#[derive(Default)]
struct Fields {
    meta: SourceMetadata,
    rating_xmp: Option<i64>,
    rating_other: Option<i64>,
    keywords: Option<Vec<String>>,
    hierarchical: Option<Vec<String>>,
    sizes: Vec<(String, Option<i64>, Option<i64>)>,
    orientation: Option<i64>,
    rotation: Option<i64>,
    duration: Option<f64>,
    mime: Option<String>,
}

impl Fields {
    fn add(&mut self, group: &str, tag: &str, value: &Value) {
        let first = |slot: &mut Option<String>| {
            if slot.is_none() {
                *slot = text_of(value)
                    .map(|text| text.trim().to_owned())
                    .filter(|t| !t.is_empty());
            }
        };
        match tag {
            "DateTimeOriginal" => first(&mut self.meta.date_time_original),
            "SubSecDateTimeOriginal" => first(&mut self.meta.sub_sec_date_time_original),
            "OffsetTimeOriginal" => first(&mut self.meta.offset_time_original),
            "CreateDate" if group == "QuickTime" => first(&mut self.meta.quicktime_create_date),
            "CreateDate" => first(&mut self.meta.create_date),
            "CreationDate" if group == "Keys" => first(&mut self.meta.quicktime_creation_date),
            "MediaCreateDate" => first(&mut self.meta.media_create_date),
            "Make" => first(&mut self.meta.make),
            "Model" => first(&mut self.meta.model),
            "Rating" => {
                let rating = number_of(value).map(|n| n.round() as i64);
                if group.starts_with("XMP") {
                    self.rating_xmp = self.rating_xmp.or(rating);
                } else {
                    self.rating_other = self.rating_other.or(rating);
                }
            }
            "Pick" => {
                self.meta.pick = self
                    .meta
                    .pick
                    .or(number_of(value).map(|n| n.round() as i64))
            }
            "Subject" | "Keywords" => {
                self.keywords
                    .get_or_insert_with(Vec::new)
                    .extend(texts_of(value));
            }
            "HierarchicalSubject" => {
                self.hierarchical
                    .get_or_insert_with(Vec::new)
                    .extend(texts_of(value));
            }
            "Orientation" => {
                self.orientation = self.orientation.or(number_of(value).map(|n| n as i64))
            }
            "Rotation" => self.rotation = self.rotation.or(number_of(value).map(|n| n as i64)),
            "Duration" => {
                if self.duration.is_none() {
                    self.duration = number_of(value).filter(|n| *n > 0.0);
                }
            }
            "MIMEType" => first(&mut self.mime),
            "ImageWidth" | "ImageHeight" => {
                let number = number_of(value).map(|n| n as i64).filter(|n| *n > 0);
                let index = self
                    .sizes
                    .iter()
                    .position(|(g, ..)| g == group)
                    .unwrap_or_else(|| {
                        self.sizes.push((group.to_owned(), None, None));
                        self.sizes.len() - 1
                    });
                if tag == "ImageWidth" {
                    self.sizes[index].1 = number;
                } else {
                    self.sizes[index].2 = number;
                }
            }
            _ => {}
        }
    }

    fn into_embedded(mut self) -> Embedded {
        self.meta.rating = self.rating_xmp.or(self.rating_other);
        let mut words: Vec<String> = self.keywords.clone().unwrap_or_default();
        if let Some(hierarchical) = &self.hierarchical {
            words.extend(
                hierarchical
                    .iter()
                    .flat_map(|path| path.split('|').map(str::to_owned)),
            );
        }
        self.meta.keywords = (self.keywords.is_some() || self.hierarchical.is_some())
            .then(|| flatten_keywords(&words, false));

        // Stills: the largest picture (never the thumbnail); videos: the first track, later.
        let usable = |group: &str| {
            !matches!(group, "IFD1" | "ThumbnailIFD" | "Thumbnail" | "Preview")
                && !group.starts_with("Preview")
        };
        let mut best: Option<(String, i64, i64)> = None;
        for (group, width, height) in &self.sizes {
            if let (Some(width), Some(height)) = (*width, *height)
                && usable(group)
            {
                let better = best.as_ref().is_none_or(|(_, w, h)| {
                    let video = group.starts_with("Track");
                    let best_video = best.as_ref().is_some_and(|(g, ..)| g.starts_with("Track"));
                    if video || best_video {
                        false
                    } else {
                        width * height > w * h
                    }
                });
                if better {
                    best = Some((group.clone(), width, height));
                }
            }
        }
        let (width, height) = best.map_or((None, None), |(_, w, h)| (Some(w), Some(h)));
        let (width, height) = {
            let rotated = matches!(self.orientation, Some(5..=8))
                || matches!(self.rotation.map(|r| r.rem_euclid(360)), Some(90 | 270));
            if rotated {
                (height, width)
            } else {
                (width, height)
            }
        };
        Embedded {
            meta: self.meta,
            width,
            height,
            duration_ms: self.duration.map(|secs| (secs * 1000.0).round() as i64),
            mime: self.mime,
        }
    }
}

impl Embedded {
    /// Final touches that depend on the kind of file.
    fn finish(&mut self, kind: FileKind) {
        if kind != FileKind::Video {
            self.duration_ms = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_options_are_read_only_and_bounded() {
        let args = base_args();
        for forbidden in [
            "-w",
            "-overwrite_original",
            "-tagsfromfile",
            "-if",
            "-p",
            "-@",
            "-csv",
            "-execute",
        ] {
            assert!(!args.iter().any(|arg| arg == forbidden), "{forbidden}");
        }
        assert!(
            args.iter()
                .all(|arg| arg.starts_with('-') || arg == "filename=utf8")
        );
        assert!(args.contains(&"-j".to_owned()) && args.contains(&"-n".to_owned()));
        assert_eq!(args.len(), 6 + TAGS.len());
    }

    #[test]
    fn unsafe_paths_are_never_argument_lines() {
        for path in ["/a/b.jpg", "/a/-x.jpg", "/a b/c.jpg", "/é/ü.jpg"] {
            assert!(safe_as_argument_line(Path::new(path)), "{path}");
        }
        for path in [
            "/a\n-w\n/x.jpg",
            "/a\r.jpg",
            "rel.jpg",
            "/a.jpg ",
            " /a.jpg",
            "/a\0.jpg",
        ] {
            assert!(!safe_as_argument_line(Path::new(path)), "{path:?}");
        }
    }

    #[test]
    fn a_photo_entry() {
        let entry = json!({
            "SourceFile": "/v/a.jpg",
            "IFD0:Make": "Canon", "IFD0:Model": "Canon EOS R5", "IFD0:Orientation": 6,
            "IFD0:Rating": 2, "XMP-xmp:Rating": 4,
            "ExifIFD:DateTimeOriginal": "2021:05:05 12:00:00", "ExifIFD:OffsetTimeOriginal": "+02:00",
            "Composite:SubSecDateTimeOriginal": "2021:05:05 12:00:00.25+02:00",
            "IFD0:CreateDate": "2021:05:05 12:00:01",
            "File:ImageWidth": 6000, "File:ImageHeight": 4000,
            "IFD1:ImageWidth": 160, "IFD1:ImageHeight": 120,
            "File:MIMEType": "image/jpeg",
            "XMP-dc:Subject": ["Paris", "trip"], "XMP-lr:HierarchicalSubject": "Places|France|Paris",
            "IPTC:Keywords": "solo"
        });
        let embedded = embedded_from_entry(&entry).unwrap();
        assert_eq!(embedded.meta.rating, Some(4));
        assert_eq!(embedded.meta.make.as_deref(), Some("Canon"));
        assert_eq!(
            embedded.meta.date_time_original.as_deref(),
            Some("2021:05:05 12:00:00")
        );
        assert_eq!(
            embedded.meta.sub_sec_date_time_original.as_deref(),
            Some("2021:05:05 12:00:00.25+02:00")
        );
        assert_eq!(
            embedded.meta.offset_time_original.as_deref(),
            Some("+02:00")
        );
        assert_eq!(
            embedded.meta.create_date.as_deref(),
            Some("2021:05:05 12:00:01")
        );
        assert_eq!(
            (embedded.width, embedded.height),
            (Some(4000), Some(6000)),
            "orientation 6 swaps"
        );
        assert_eq!(embedded.mime.as_deref(), Some("image/jpeg"));
        assert_eq!(
            embedded.meta.keywords,
            Some(
                ["solo", "Paris", "trip", "Places", "France"]
                    .map(String::from)
                    .to_vec()
            )
        );
    }

    #[test]
    fn a_video_entry() {
        let entry = json!({
            "SourceFile": "/v/a.mov",
            "QuickTime:CreateDate": "2022:01:02 03:04:05", "Keys:CreationDate": "2022:01:02 04:04:05+01:00",
            "Track1:MediaCreateDate": "2022:01:02 03:04:05",
            "Track1:ImageWidth": 1920, "Track1:ImageHeight": 1080, "QuickTime:Rotation": 90,
            "QuickTime:Duration": 12.3456, "Keys:Make": "Apple", "Keys:Model": "iPhone 15",
            "File:MIMEType": "video/quicktime"
        });
        let embedded = embedded_from_entry(&entry).unwrap();
        assert_eq!(
            embedded.meta.quicktime_create_date.as_deref(),
            Some("2022:01:02 03:04:05")
        );
        assert_eq!(embedded.meta.create_date, None);
        assert_eq!(
            embedded.meta.quicktime_creation_date.as_deref(),
            Some("2022:01:02 04:04:05+01:00")
        );
        assert_eq!(
            embedded.meta.media_create_date.as_deref(),
            Some("2022:01:02 03:04:05")
        );
        assert_eq!((embedded.width, embedded.height), (Some(1080), Some(1920)));
        assert_eq!(embedded.duration_ms, Some(12346));
    }

    #[test]
    fn errors_and_empty_entries() {
        let failed = embedded_from_entry(
            &json!({"SourceFile": "/a", "ExifTool:Error": "File format error"}),
        );
        assert_eq!(
            failed,
            Err(ExtractFailure::Unreadable("File format error".into()))
        );
        let bare = embedded_from_entry(&json!({"SourceFile": "/a"})).unwrap();
        assert_eq!(bare, Embedded::default());
        let numeric =
            embedded_from_entry(&json!({"SourceFile": "/a", "IFD0:Make": 123, "IFD0:Model": ""}))
                .unwrap();
        assert_eq!(numeric.meta.make.as_deref(), Some("123"));
        assert_eq!(numeric.meta.model, None);
    }

    #[test]
    fn answers_are_matched_by_source_file() {
        let bytes = br#"[{"SourceFile":"/a.jpg","IFD0:Make":"X"},{"SourceFile":"/b.jpg"}]"#;
        let entries = parse_output(bytes).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(parse_output(b"").unwrap().is_empty());
        assert!(parse_output(b"garbage").is_err());
    }
}
