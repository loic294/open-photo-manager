//! Shared by the query tests: an in-memory model of the rows seeded into the database, with a
//! plain-Rust reference for every query (scope, kind, filter, sort, folders, footer counts,
//! facets), and the library the tests run over.
//!
//! The references use `Scope::contains`, `Filter::matches` and `normalize_name` from `opm-core`,
//! never the SQL under test, so a disagreement is a bug on one side or the other.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet};

use opm_core::dto::{FileKind, Filter, FlagFilter, Scope, SortKey, SortOrder};
use opm_core::filter::FileFacts;
use opm_core::kind::classify_name;
use opm_core::names::normalize_name;
use opm_core::paths::{FileId, RelPath, SelectionId, VolumeId};
use opm_server::db::{Db, TempDb};
use opm_server::query::{FileQuery, KindFilter, QueryScope};

use crate::common::seed::{SIDECAR_EXTENSIONS, Seed};

/// One seeded file, as the reference sees it.
#[derive(Clone, Debug)]
pub struct Row {
    pub id: FileId,
    pub volume_id: VolumeId,
    pub dir: String,
    pub name: String,
    pub kind: FileKind,
    pub size: i64,
    pub capture: Option<i64>,
    pub camera: Option<String>,
    pub rating: u8,
    pub flag: Option<FlagFilter>,
    pub tags: Vec<String>,
}

/// What a test chooses about a file it adds; everything else is the seeding default.
#[derive(Clone, Debug, Default)]
pub struct Spec {
    /// `None`: the kind of the extension.
    pub kind: Option<FileKind>,
    pub size: i64,
    pub capture: Option<i64>,
    pub camera: Option<&'static str>,
    pub rating: u8,
    pub flag: Option<FlagFilter>,
    pub tags: Vec<&'static str>,
}

/// The expected shape of one entry of the folder list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderExpect {
    pub name: String,
    pub path: String,
    pub volume_id: VolumeId,
    pub count: i64,
    pub total: i64,
}

/// The expected facets, as `(value, count)` lists in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FacetsExpect {
    pub cameras: Vec<(String, i64)>,
    pub tags: Vec<(String, i64)>,
    pub ratings: Vec<(String, i64)>,
}

/// The seeded rows, the directories that exist and the selections.
#[derive(Default)]
pub struct Model {
    pub rows: Vec<Row>,
    pub dirs: BTreeSet<(VolumeId, String)>,
    pub selections: BTreeMap<SelectionId, BTreeSet<FileId>>,
}

/// A database with its model. `photos` and `backup` are two volumes.
pub struct Fixture {
    pub db: TempDb,
    pub photos: VolumeId,
    pub backup: VolumeId,
    pub model: Model,
}

impl Fixture {
    /// Two empty volumes.
    pub async fn empty() -> Self {
        let db = Db::open_temp().await.unwrap();
        let seed = Seed::new(&db);
        let photos = seed.volume("photos").await;
        let backup = seed.volume("backup").await;
        let mut model = Model::default();
        model.dirs.insert((photos, String::new()));
        model.dirs.insert((backup, String::new()));
        Self {
            db,
            photos,
            backup,
            model,
        }
    }

    /// The generated library (see [`populate`]).
    pub async fn library() -> Self {
        let mut fixture = Self::empty().await;
        populate(&mut fixture).await;
        fixture
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub async fn add(&mut self, volume_id: VolumeId, dir: &str, name: &str, spec: Spec) -> FileId {
        let mut file = Seed::new(&self.db)
            .file(volume_id, dir, name)
            .size(spec.size)
            .rating(spec.rating)
            .tags(spec.tags.iter().copied());
        if let Some(kind) = spec.kind {
            file = file.kind(kind);
        }
        if let Some(capture) = spec.capture {
            file = file.capture(capture);
        }
        if let Some(camera) = spec.camera {
            file = file.camera(camera);
        }
        if let Some(flag) = spec.flag {
            file = file.flag(flag);
        }
        let id = file.insert().await;
        self.note_dir(volume_id, dir);
        self.model.rows.push(Row {
            id,
            volume_id,
            dir: dir.to_owned(),
            name: name.to_owned(),
            kind: spec
                .kind
                .unwrap_or_else(|| classify_name(name, SIDECAR_EXTENSIONS)),
            size: spec.size,
            capture: spec.capture,
            camera: spec.camera.map(str::to_owned),
            rating: spec.rating,
            flag: spec.flag,
            tags: spec.tags.iter().map(|tag| (*tag).to_owned()).collect(),
        });
        id
    }

    /// A directory without files (and its ancestors).
    pub async fn add_dir(&mut self, volume_id: VolumeId, dir: &str) {
        Seed::new(&self.db).dir(volume_id, dir).await;
        self.note_dir(volume_id, dir);
    }

    pub async fn select(&mut self, ids: &[FileId]) -> SelectionId {
        let id = Seed::new(&self.db).selection(ids).await;
        self.model
            .selections
            .insert(id, ids.iter().copied().collect());
        id
    }

    fn note_dir(&mut self, volume_id: VolumeId, dir: &str) {
        let mut current = String::new();
        for component in dir.split('/').filter(|component| !component.is_empty()) {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(component);
            self.model.dirs.insert((volume_id, current.clone()));
        }
    }
}

/// Directories of the library on volume `photos`. Several pairs share a prefix, and some names
/// hold characters that matter to `LIKE` or to SQL quoting.
pub const PHOTO_DIRS: &[&str] = &[
    "",
    "2024",
    "2024-05",
    "2024/05",
    "2024/05/day 1",
    "2024/06",
    "2023",
    "a/b",
    "a/b-c",
    "a/b/c",
    "100%_done",
    "it's",
    "quote\"d",
    "with space",
    "\u{e9}t\u{e9}",
    "e\u{301}te\u{301}",
    "Alpha",
    "alpha",
    "x/y/z",
];

/// Directories of the library on volume `backup`.
pub const BACKUP_DIRS: &[&str] = &["", "2024", "2024/05", "a/b", "extra"];

/// Empty directories added to both volumes.
pub const EMPTY_DIRS: &[&str] = &["2024/empty/deeper", "a/b/empty", "zzz-empty"];

const NAMES: [&str; 8] = [
    "IMG_0001.jpg",
    "IMG_0002.CR3",
    "clip.mp4",
    "Song.wav",
    "notes.txt",
    "IMG_0001.jpg.xmp",
    "IMG_0002.xmp",
    "Zebra.JPG",
];

pub const CANON: &str = "Canon EOS R5";
pub const SONY: &str = "Sony ILCE-7M4";
pub const NIKON: &str = "Nikon Z 6";
pub const QUOTE_CAMERA: &str = "x' OR '1'='1";
pub const WILD_CAMERA: &str = "Fuji \"X-T4\" 100%_";
pub const DROP_TAG: &str = "'; DROP TABLE files; --";

const CAMERAS: [Option<&str>; 7] = [
    Some(CANON),
    Some(SONY),
    Some(NIKON),
    Some(QUOTE_CAMERA),
    Some(WILD_CAMERA),
    Some("Leica Q3"),
    None,
];

const TAG_SETS: [&[&str]; 5] = [
    &[],
    &["holiday"],
    &["holiday", "family"],
    &["Work", DROP_TAG],
    &["%", "_", "a b"],
];

/// The properties of the `n`-th generated file: spread over cameras, ratings, flags, tags, capture
/// times (some missing, many equal) and sizes (some equal).
fn generated_spec(n: usize) -> Spec {
    let group = n / NAMES.len();
    Spec {
        kind: None,
        size: i64::try_from((n * 7919) % 1000 + 1).unwrap(),
        capture: (n % 4 != 3)
            .then(|| 1_700_000_000 + i64::try_from((n * 37) % 11).unwrap() * 86_400),
        camera: CAMERAS[(n * 3 + group) % CAMERAS.len()],
        rating: u8::try_from((n * 5 + group) % 6).unwrap(),
        flag: match (n + group) % 4 {
            0 => Some(FlagFilter::Selected),
            1 => Some(FlagFilter::Rejected),
            _ => None,
        },
        tags: TAG_SETS[n % TAG_SETS.len()].to_vec(),
    }
}

/// Eight files in every directory of both volumes (the same names everywhere, so names tie across
/// directories), plus the empty directories.
pub async fn populate(fixture: &mut Fixture) {
    let mut n = 0;
    for (volume_id, dirs) in [(fixture.photos, PHOTO_DIRS), (fixture.backup, BACKUP_DIRS)] {
        for dir in dirs {
            for name in NAMES {
                fixture.add(volume_id, dir, name, generated_spec(n)).await;
                n += 1;
            }
        }
    }
    for volume_id in [fixture.photos, fixture.backup] {
        for dir in EMPTY_DIRS {
            fixture.add_dir(volume_id, dir).await;
        }
    }
}

pub fn scope(volume_id: Option<VolumeId>, dir: &str, recursive: bool) -> Scope {
    Scope {
        volume_id,
        dir: dir.to_owned(),
        recursive,
    }
}

pub fn filter(
    cameras: &[&str],
    ratings: &[u8],
    flags: &[FlagFilter],
    tags: &[&str],
    selection_id: Option<SelectionId>,
) -> Filter {
    Filter {
        cameras: cameras.iter().map(|name| (*name).to_owned()).collect(),
        ratings: ratings.to_vec(),
        flags: flags.to_vec(),
        tags: tags.iter().map(|name| (*name).to_owned()).collect(),
        selection_id,
    }
}

/// A query over `scope` and `filter`, as the callers build it.
pub fn query(scope: &Scope, filter: &Filter, kinds: KindFilter) -> FileQuery {
    FileQuery::new(QueryScope::try_from(scope).unwrap(), filter)
        .unwrap()
        .kinds(kinds)
}

/// Every scope worth testing: each volume and every volume, a spread of directories (prefix
/// siblings, special characters, a Unicode pair, one that does not exist), recursive or not.
pub fn scope_matrix(fixture: &Fixture) -> Vec<Scope> {
    let dirs = [
        "",
        "2024",
        "2024-05",
        "2024/05",
        "2024/05/day 1",
        "a/b",
        "a/b-c",
        "100%_done",
        "it's",
        "quote\"d",
        "\u{e9}t\u{e9}",
        "e\u{301}te\u{301}",
        "nowhere",
    ];
    let mut scopes = Vec::new();
    for volume_id in [Some(fixture.photos), Some(fixture.backup), None] {
        for dir in dirs {
            for recursive in [true, false] {
                scopes.push(scope(volume_id, dir, recursive));
            }
        }
    }
    scopes
}

/// A filter of the matrix: its label, the filter, and whether an empty result is expected from the
/// library (it is for the filters that name nothing that exists).
pub struct MatrixFilter {
    pub label: &'static str,
    pub filter: Filter,
    pub may_be_empty: bool,
}

/// The filters worth testing: each dimension alone, combined, with other spellings, with
/// injection-looking values and with selections. Adds the selections it needs to the fixture.
pub async fn filter_matrix(fixture: &mut Fixture) -> Vec<MatrixFilter> {
    let third_of_the_files: Vec<FileId> = fixture
        .model
        .rows
        .iter()
        .map(|row| row.id)
        .filter(|id| id % 3 == 0)
        .collect();
    let third = fixture.select(&third_of_the_files).await;
    let empty = fixture.select(&[]).await;

    let selected = [FlagFilter::Selected];
    let rejected = [FlagFilter::Rejected];
    let both = [FlagFilter::Selected, FlagFilter::Rejected];
    let cases: Vec<(&'static str, Filter, bool)> = vec![
        ("none", Filter::default(), false),
        (
            "camera, other case",
            filter(&["canon eos r5"], &[], &[], &[], None),
            false,
        ),
        (
            "two cameras",
            filter(&[CANON, "SONY ILCE-7M4"], &[], &[], &[], None),
            false,
        ),
        (
            "unknown camera",
            filter(&["Nobody Cam"], &[], &[], &[], None),
            true,
        ),
        ("unrated", filter(&[], &[0], &[], &[], None), false),
        ("rated high", filter(&[], &[4, 5], &[], &[], None), false),
        (
            "unrated or three",
            filter(&[], &[0, 3], &[], &[], None),
            false,
        ),
        ("selected", filter(&[], &[], &selected, &[], None), false),
        ("rejected", filter(&[], &[], &rejected, &[], None), false),
        (
            "selected or rejected",
            filter(&[], &[], &both, &[], None),
            false,
        ),
        ("tag", filter(&[], &[], &[], &["holiday"], None), false),
        (
            "tags, other case",
            filter(&[], &[], &[], &["HOLIDAY", "work"], None),
            false,
        ),
        (
            "camera and rating",
            filter(&[CANON], &[3, 4, 5], &[], &[], None),
            false,
        ),
        (
            "everything",
            filter(
                &[CANON, NIKON],
                &[1, 2, 3, 4, 5],
                &both,
                &["holiday", "family", "Work"],
                None,
            ),
            false,
        ),
        (
            "injection in a tag",
            filter(&[], &[], &[], &[DROP_TAG], None),
            false,
        ),
        (
            "injection in a camera",
            filter(&[QUOTE_CAMERA], &[], &[], &[], None),
            false,
        ),
        ("percent tag", filter(&[], &[], &[], &["%"], None), false),
        ("underscore tag", filter(&[], &[], &[], &["_"], None), false),
        (
            "special camera",
            filter(&[WILD_CAMERA], &[], &[], &[], None),
            false,
        ),
        ("selection", filter(&[], &[], &[], &[], Some(third)), false),
        (
            "selection and tag",
            filter(&[], &[], &[], &["holiday"], Some(third)),
            false,
        ),
        (
            "empty selection",
            filter(&[], &[], &[], &[], Some(empty)),
            true,
        ),
        (
            "unknown selection",
            filter(&[], &[], &[], &[], Some(1_000_000)),
            true,
        ),
        (
            "blank entries",
            filter(&["  "], &[], &[], &[""], None),
            false,
        ),
        (
            "padded duplicates",
            filter(&[], &[], &[], &[" holiday ", "HOLIDAY"], None),
            false,
        ),
    ];
    cases
        .into_iter()
        .map(|(label, filter, may_be_empty)| MatrixFilter {
            label,
            filter,
            may_be_empty,
        })
        .collect()
}

impl Model {
    /// `filter` must be in canonical form (the engine normalises the filter it is given).
    fn passes_filter(&self, row: &Row, filter: &Filter) -> bool {
        let facts = FileFacts {
            camera: row.camera.clone(),
            rating: row.rating,
            flag: row.flag,
            tags: row.tags.clone(),
        };
        let in_selection = match filter.selection_id {
            None => true,
            Some(id) => self
                .selections
                .get(&id)
                .is_some_and(|members| members.contains(&row.id)),
        };
        filter.matches(&facts) && in_selection
    }

    fn in_scope(row: &Row, scope: &Scope) -> bool {
        scope.contains(row.volume_id, &RelPath::new(&row.dir).unwrap())
    }

    fn visible(row: &Row, kinds: KindFilter) -> bool {
        kinds == KindFilter::All || row.kind != FileKind::Sidecar
    }

    /// The rows of the scope that have a listed kind and pass the filter, in insertion order.
    pub fn matching(&self, scope: &Scope, filter: &Filter, kinds: KindFilter) -> Vec<&Row> {
        let filter = filter.normalise().unwrap();
        self.rows
            .iter()
            .filter(|row| {
                Self::in_scope(row, scope)
                    && Self::visible(row, kinds)
                    && self.passes_filter(row, &filter)
            })
            .collect()
    }

    pub fn expected_ids(
        &self,
        scope: &Scope,
        filter: &Filter,
        kinds: KindFilter,
        sort: SortKey,
        order: SortOrder,
    ) -> Vec<FileId> {
        let mut rows = self.matching(scope, filter, kinds);
        rows.sort_by(|a, b| compare(a, b, sort, order));
        rows.iter().map(|row| row.id).collect()
    }

    /// The direct subfolders of the scope's directory with their counts.
    pub fn folders(&self, scope: &Scope, filter: &Filter, kinds: KindFilter) -> Vec<FolderExpect> {
        let volume_id = scope.volume_id.expect("folders need a volume");
        let parent = scope_dir(scope);
        let mut folders: Vec<FolderExpect> = self
            .dirs
            .iter()
            .filter(|(volume, path)| *volume == volume_id && !path.is_empty())
            .filter(|(_, path)| path.rsplit_once('/').map_or("", |(up, _)| up) == parent)
            .map(|(_, path)| {
                let below = self::scope(Some(volume_id), path, true);
                let total = self.matching(&below, &Filter::default(), kinds).len();
                let count = self.matching(&below, filter, kinds).len();
                FolderExpect {
                    name: path.rsplit('/').next().unwrap().to_owned(),
                    path: path.clone(),
                    volume_id,
                    count: i64::try_from(count).unwrap(),
                    total: i64::try_from(total).unwrap(),
                }
            })
            .collect();
        folders.sort_by_key(|folder| (normalize_name(&folder.name), folder.name.clone()));
        folders
    }

    /// `(in_folder, in_subfolders)` for the scope's directory, whatever `scope.recursive` says.
    pub fn footer(&self, scope: &Scope, filter: &Filter, kinds: KindFilter) -> (i64, i64) {
        let below = Scope {
            recursive: true,
            ..scope.clone()
        };
        let rows = self.matching(&below, filter, kinds);
        let dir = scope_dir(scope);
        let in_folder = rows.iter().filter(|row| row.dir == dir).count();
        (
            i64::try_from(in_folder).unwrap(),
            i64::try_from(rows.len() - in_folder).unwrap(),
        )
    }

    /// Each dimension counted without its own filter.
    pub fn facets(&self, scope: &Scope, filter: &Filter, kinds: KindFilter) -> FacetsExpect {
        let without_cameras = Filter {
            cameras: Vec::new(),
            ..filter.clone()
        };
        let without_tags = Filter {
            tags: Vec::new(),
            ..filter.clone()
        };
        let without_ratings = Filter {
            ratings: Vec::new(),
            ..filter.clone()
        };

        let mut cameras: BTreeMap<String, i64> = BTreeMap::new();
        for row in self.matching(scope, &without_cameras, kinds) {
            if let Some(camera) = &row.camera {
                *cameras.entry(camera.clone()).or_default() += 1;
            }
        }
        let mut tags: BTreeMap<String, i64> = BTreeMap::new();
        for row in self.matching(scope, &without_tags, kinds) {
            for tag in &row.tags {
                *tags.entry(tag.clone()).or_default() += 1;
            }
        }
        let mut ratings: BTreeMap<String, i64> = BTreeMap::new();
        for row in self.matching(scope, &without_ratings, kinds) {
            *ratings.entry(row.rating.to_string()).or_default() += 1;
        }
        FacetsExpect {
            cameras: ordered(cameras),
            tags: ordered(tags),
            ratings: ordered(ratings),
        }
    }
}

fn scope_dir(scope: &Scope) -> String {
    RelPath::new(&scope.dir).unwrap().as_str().to_owned()
}

/// Count descending, then the value ignoring ASCII case (the collation of the name columns).
fn ordered(counts: BTreeMap<String, i64>) -> Vec<(String, i64)> {
    let mut list: Vec<(String, i64)> = counts.into_iter().collect();
    list.sort_by_key(|(value, count)| (Reverse(*count), value.to_ascii_lowercase()));
    list
}

fn directed(ordering: Ordering, order: SortOrder) -> Ordering {
    match order {
        SortOrder::Asc => ordering,
        SortOrder::Desc => ordering.reverse(),
    }
}

/// The documented order: the primary key (reversed by `desc`; files without a capture time come
/// last either way), then the normalised name, then the id.
fn compare(a: &Row, b: &Row, sort: SortKey, order: SortOrder) -> Ordering {
    let by_name = normalize_name(&a.name).cmp(&normalize_name(&b.name));
    let primary = match sort {
        SortKey::Name => directed(by_name, order),
        SortKey::Date => match (a.capture, b.capture) {
            (Some(x), Some(y)) => directed(x.cmp(&y), order),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
        SortKey::Size => directed(a.size.cmp(&b.size), order),
    };
    primary.then(by_name).then(a.id.cmp(&b.id))
}
