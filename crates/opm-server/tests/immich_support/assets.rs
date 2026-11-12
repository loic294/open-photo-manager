//! The assets of the fake Immich beyond the search: reading one asset, changing and deleting assets
//! in bulk, albums, tags and the scan of a library over real directories. It is what the outbox of
//! file operations (`immich/outbox/`) calls, modelled like `tools/mock-immich` models it for a real
//! HTTP server: change a shape or a behaviour in both.
//!
//! - `GET /assets/{id}` shows the favorite flag, the rating and description (`exifInfo`), the tags and
//!   `hasMetadata`. An id Immich does not know answers 400 `Not found or no asset.read access` (404
//!   after [`FakeImmich::unknown_asset_status`]).
//! - `PUT /assets` sets `isFavorite`, `rating` (-1 to 5, `null` clears), `visibility` and
//!   `description` on every id, all or nothing: one unknown id refuses the whole call.
//! - `DELETE /assets` sends the assets to Immich's trash, or removes them for good with `force`.
//!   Forcing the delete of an asset that is ONLINE removes its original file in a real Immich: the
//!   fake records a [`Violation`] (and removes the file when a mount says where it lives). Every
//!   test that drives the application asserts [`FakeImmich::assert_no_violations`].
//! - `GET /albums?assetId=`, `PUT /albums/{id}/assets` (one result per asset: `success`, or the
//!   error `duplicate` or `no_permission`), `GET /tags` and `PUT /tags/assets` (`count` is tags
//!   times assets).
//! - `POST /libraries/{id}/scan` (administrators only, 204) walks the real directories behind the
//!   import paths of the library, mounted with [`FakeImmich::mount`]: hidden entries, files that
//!   are not media and what the exclusion patterns match are skipped. A file Immich has no asset
//!   for becomes one (not a favorite, in no album, without tags); an offline asset whose file came
//!   back is online again; an online asset whose file is gone turns offline and trashed. An import
//!   path no mount covers is not looked at and its assets are left alone.
//!
//! What a test shapes: [`FakeImmich::mount`], [`FakeImmich::scan_library`] (what Immich does when
//! its own job runs), [`FakeImmich::hold_scans`] and [`FakeImmich::run_held_scans`] (a scan that is
//! asked for now and runs later), [`FakeImmich::slow_imports`] (new assets have no metadata yet),
//! albums, tags and the exclusion patterns of a library.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{Asset, FakeImmich, Reply, State, TIMESTAMP, Who, failure, needs, new_id, next_number};

/// The visibilities Immich knows.
const VISIBILITIES: [&str; 4] = ["archive", "timeline", "hidden", "locked"];

/// The extensions of the files a library scan imports.
const MEDIA: &str = "3fr ari arw avif bmp cap cin cr2 cr3 crw dcr dng erf fff gif heic heif hif \
    iiq jp2 jpe jpeg jpg jxl k25 kdc mrw nef orf ori pef png psd raf raw rw2 rwl sr2 srf srw svg \
    tif tiff webp x3f 3gp 3gpp avi flv m2t m2ts m4v mkv mov mp4 mpg mts mxf ts webm wmv";

#[derive(Clone, Debug)]
pub struct Album {
    pub id: String,
    pub name: String,
    pub asset_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Tag {
    pub id: String,
    pub name: String,
    /// The whole path of the tag, `Parent/Child`.
    pub value: String,
}

/// A request that would have destroyed an original file in a real Immich: a forced delete of an
/// asset that is online.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub asset_id: String,
    pub path: String,
    /// The fake removed the file, because a mount says where it lives.
    pub file_removed: bool,
}

/// What the fake keeps besides the assets and the libraries.
pub(super) struct World {
    pub(super) albums: Vec<Album>,
    pub(super) tags: Vec<Tag>,
    pub(super) violations: Vec<Violation>,
    /// `(path prefix on the Immich server, real directory)`: how a scan reaches the files.
    mounts: Vec<(String, PathBuf)>,
    unknown_asset_status: u16,
    hold_scans: bool,
    /// The libraries whose scan was asked for while scans were held.
    held: Vec<String>,
    slow_imports: bool,
}

impl Default for World {
    fn default() -> Self {
        Self {
            albums: Vec::new(),
            tags: Vec::new(),
            violations: Vec::new(),
            mounts: Vec::new(),
            unknown_asset_status: 400,
            hold_scans: false,
            held: Vec::new(),
            slow_imports: false,
        }
    }
}

impl World {
    /// The real path behind a path of the Immich server, when a mount covers it.
    fn real_path(&self, immich_path: &str) -> Option<PathBuf> {
        let (prefix, dir) = self
            .mounts
            .iter()
            .filter(|(prefix, _)| is_under(immich_path, prefix))
            .max_by_key(|(prefix, _)| prefix.len())?;
        let rest = immich_path[prefix.len()..].trim_start_matches('/');
        if rest.split('/').any(|part| part == "..") {
            return None;
        }
        Some(if rest.is_empty() {
            dir.clone()
        } else {
            dir.join(rest)
        })
    }
}

fn trim_slash(path: &str) -> &str {
    if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    }
}

/// Whether `path` is `root` or lies below it, compared on whole segments.
fn is_under(path: &str, root: &str) -> bool {
    let base = trim_slash(root);
    base == "/"
        || path == base
        || path
            .strip_prefix(base)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn bad(message: &str) -> Reply {
    failure(400, "Bad Request", message)
}

/// The request may name only `allowed` properties.
fn only(body: &Value, allowed: &[&str]) -> Result<(), Reply> {
    let Some(object) = body.as_object() else {
        return Err(bad("the body must be an object"));
    };
    match object.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(bad(&format!("property {key} should not exist"))),
        None => Ok(()),
    }
}

fn id_list(body: &Value, field: &str) -> Result<Vec<String>, Reply> {
    let Some(items) = body[field].as_array() else {
        return Err(bad(&format!("{field} must be an array")));
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| bad(&format!("each value in {field} must be a string")))
        })
        .collect()
}

fn optional_bool(body: &Value, field: &str) -> Result<Option<bool>, Reply> {
    match body.get(field) {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(bad(&format!("{field} must be a boolean value"))),
    }
}

fn optional_text(body: &Value, field: &str) -> Result<Option<String>, Reply> {
    match body.get(field) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(bad(&format!("{field} must be a string"))),
    }
}

/// `None`: not in the request. `Some(None)`: cleared. `Some(Some(stars))`: set.
fn optional_rating(body: &Value) -> Result<Option<Option<i32>>, Reply> {
    match body.get("rating") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(value) => value
            .as_i64()
            .filter(|stars| (-1..=5).contains(stars))
            .and_then(|stars| i32::try_from(stars).ok())
            .map(|stars| Some(Some(stars)))
            .ok_or_else(|| bad("rating must not be greater than 5 or less than -1")),
    }
}

impl State {
    /// An asset an API key may reach: not a locked one.
    fn reachable(&self, id: &str) -> Option<&Asset> {
        self.assets
            .iter()
            .find(|asset| asset.id == id && asset.visibility != "locked")
    }

    /// Immich refuses the whole call when one id is out of reach.
    fn require_assets(&self, ids: &[String], access: &str) -> Result<(), Reply> {
        match ids.iter().find(|id| self.reachable(id).is_none()) {
            Some(_) => Err(bad(&format!("Not found or no {access} access"))),
            None => Ok(()),
        }
    }

    pub(super) fn get_asset(&self, who: &Who, id: &str) -> Reply {
        if let Some(refusal) = self.guard(who, needs("asset.read")) {
            return refusal;
        }
        match self.reachable(id) {
            Some(asset) => (200, Some(self.asset_view(asset))),
            None => failure(
                self.world.unknown_asset_status,
                if self.world.unknown_asset_status == 404 {
                    "Not Found"
                } else {
                    "Bad Request"
                },
                "Not found or no asset.read access",
            ),
        }
    }

    /// The whole asset of `GET /assets/{id}`.
    fn asset_view(&self, asset: &Asset) -> Value {
        let mut view = self.asset_json(asset);
        view["hasMetadata"] = json!(asset.has_metadata);
        view["exifInfo"] = json!({
            "rating": asset.rating,
            "description": asset.description,
        });
        let tags: Vec<Value> = asset
            .tags
            .iter()
            .filter_map(|id| self.world.tags.iter().find(|tag| &tag.id == id))
            .map(tag_json)
            .collect();
        view["tags"] = Value::Array(tags);
        view
    }

    pub(super) fn update_assets(&mut self, who: &Who, body: &Value) -> Reply {
        if let Some(refusal) = self.guard(who, needs("asset.update")) {
            return refusal;
        }
        match self.try_update_assets(body) {
            Ok(()) => (204, None),
            Err(refusal) => refusal,
        }
    }

    fn try_update_assets(&mut self, body: &Value) -> Result<(), Reply> {
        only(
            body,
            &["ids", "isFavorite", "rating", "visibility", "description"],
        )?;
        let ids = id_list(body, "ids")?;
        let favorite = optional_bool(body, "isFavorite")?;
        let rating = optional_rating(body)?;
        let visibility = optional_text(body, "visibility")?;
        if let Some(visibility) = &visibility
            && !VISIBILITIES.contains(&visibility.as_str())
        {
            return Err(bad(
                "visibility must be one of the following values: archive, timeline, hidden, locked",
            ));
        }
        let description = optional_text(body, "description")?;
        self.require_assets(&ids, "asset.update")?;
        let now = self.search.clock_ms;
        for asset in self
            .assets
            .iter_mut()
            .filter(|asset| ids.contains(&asset.id))
        {
            if let Some(favorite) = favorite {
                asset.is_favorite = favorite;
            }
            if let Some(rating) = rating {
                asset.rating = rating;
            }
            if let Some(visibility) = &visibility {
                asset.visibility.clone_from(visibility);
            }
            if let Some(description) = &description {
                asset.description = Some(description.clone());
            }
            asset.updated_at_ms = now;
        }
        Ok(())
    }

    pub(super) fn delete_assets(&mut self, who: &Who, body: &Value) -> Reply {
        if let Some(refusal) = self.guard(who, needs("asset.delete")) {
            return refusal;
        }
        match self.try_delete_assets(body) {
            Ok(()) => (204, None),
            Err(refusal) => refusal,
        }
    }

    fn try_delete_assets(&mut self, body: &Value) -> Result<(), Reply> {
        only(body, &["ids", "force"])?;
        let ids = id_list(body, "ids")?;
        let force = optional_bool(body, "force")?.unwrap_or(false);
        self.require_assets(&ids, "asset.delete")?;
        let now = self.search.clock_ms;
        for id in &ids {
            if force {
                self.remove_for_good(id);
            } else if let Some(asset) = self.assets.iter_mut().find(|asset| &asset.id == id) {
                asset.is_trashed = true;
                asset.updated_at_ms = now;
            }
        }
        Ok(())
    }

    /// A forced delete: the asset is gone with its albums. When the asset was online, a real
    /// Immich removes the original file: the fake does too, when it can reach it, and records it.
    fn remove_for_good(&mut self, id: &str) {
        let Some(position) = self.assets.iter().position(|asset| asset.id == id) else {
            return;
        };
        let asset = self.assets.remove(position);
        if !asset.is_offline {
            let file_removed = self
                .world
                .real_path(&asset.original_path)
                .is_some_and(|real| std::fs::remove_file(real).is_ok());
            self.world.violations.push(Violation {
                asset_id: asset.id.clone(),
                path: asset.original_path,
                file_removed,
            });
        }
        for album in &mut self.world.albums {
            album.asset_ids.retain(|held| held != id);
        }
    }

    pub(super) fn list_albums(&self, who: &Who, query: &[(String, String)]) -> Reply {
        if let Some(refusal) = self.guard(who, needs("album.read")) {
            return refusal;
        }
        let asset_id = query
            .iter()
            .find(|(key, _)| key == "assetId")
            .map(|(_, value)| value.as_str());
        let albums: Vec<Value> = self
            .world
            .albums
            .iter()
            .filter(|album| asset_id.is_none_or(|id| album.asset_ids.iter().any(|held| held == id)))
            .map(|album| self.album_json(album))
            .collect();
        (200, Some(Value::Array(albums)))
    }

    fn album_json(&self, album: &Album) -> Value {
        json!({
            "id": album.id,
            "albumName": album.name,
            "description": "",
            "assetCount": album.asset_ids.len(),
            "shared": false,
            "ownerId": self.user_id,
            "createdAt": TIMESTAMP,
            "updatedAt": TIMESTAMP,
            "albumUsers": [],
            "hasSharedLink": false,
            "isActivityEnabled": true,
        })
    }

    pub(super) fn add_to_album(&mut self, who: &Who, id: &str, body: &Value) -> Reply {
        if let Some(refusal) = self.guard(who, needs("albumAsset.create")) {
            return refusal;
        }
        if let Err(refusal) = only(body, &["ids"]) {
            return refusal;
        }
        let ids = match id_list(body, "ids") {
            Ok(ids) => ids,
            Err(refusal) => return refusal,
        };
        let Some(position) = self.world.albums.iter().position(|album| album.id == id) else {
            return bad("Not found or no albumAsset.create access");
        };
        let results: Vec<Value> = ids
            .into_iter()
            .map(|asset_id| {
                if self.reachable(&asset_id).is_none() {
                    return json!({ "id": asset_id, "success": false, "error": "no_permission" });
                }
                let album = &mut self.world.albums[position];
                if album.asset_ids.contains(&asset_id) {
                    return json!({ "id": asset_id, "success": false, "error": "duplicate" });
                }
                album.asset_ids.push(asset_id.clone());
                json!({ "id": asset_id, "success": true })
            })
            .collect();
        (200, Some(Value::Array(results)))
    }

    pub(super) fn list_tags(&self, who: &Who) -> Reply {
        if let Some(refusal) = self.guard(who, needs("tag.read")) {
            return refusal;
        }
        let tags: Vec<Value> = self.world.tags.iter().map(tag_json).collect();
        (200, Some(Value::Array(tags)))
    }

    pub(super) fn tag_assets(&mut self, who: &Who, body: &Value) -> Reply {
        if let Some(refusal) = self.guard(who, needs("tag.asset")) {
            return refusal;
        }
        if let Err(refusal) = only(body, &["assetIds", "tagIds"]) {
            return refusal;
        }
        let (asset_ids, tag_ids) = match (id_list(body, "assetIds"), id_list(body, "tagIds")) {
            (Ok(assets), Ok(tags)) => (assets, tags),
            (Err(refusal), _) | (_, Err(refusal)) => return refusal,
        };
        let known: Vec<String> = tag_ids
            .iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|id| self.world.tags.iter().any(|tag| &tag.id == *id))
            .cloned()
            .collect();
        let mut reached = 0;
        for asset in self
            .assets
            .iter_mut()
            .filter(|asset| asset_ids.contains(&asset.id) && asset.visibility != "locked")
        {
            reached += 1;
            for tag in &known {
                if !asset.tags.contains(tag) {
                    asset.tags.push(tag.clone());
                }
            }
        }
        (200, Some(json!({ "count": known.len() * reached })))
    }

    pub(super) fn request_scan(&mut self, who: &Who, id: &str) -> Reply {
        if let Some(refusal) = self.guard(who, needs("library.update").admin()) {
            return refusal;
        }
        if !self.libraries.iter().any(|library| library.id == id) {
            return bad("Not found or no library.update access");
        }
        if self.world.hold_scans {
            self.world.held.push(id.to_owned());
        } else {
            self.scan(id);
        }
        (204, None)
    }

    /// What Immich does when the scan of a library runs.
    pub(super) fn scan(&mut self, library_id: &str) {
        let Some(library) = self
            .libraries
            .iter()
            .find(|library| library.id == library_id)
            .cloned()
        else {
            return;
        };
        let exclusions: Vec<Vec<Part>> = library
            .exclusion_patterns
            .iter()
            .map(|pattern| compile(pattern))
            .collect();
        let mut found = BTreeSet::new();
        let mut scanned = Vec::new();
        for root in &library.import_paths {
            let base = trim_slash(root);
            let Some(real) = self.world.real_path(base) else {
                continue;
            };
            scanned.push(root.clone());
            if !excluded(&exclusions, base) {
                walk(&real, base, &exclusions, &mut found);
            }
        }
        let now = self.search.clock_ms;
        for path in &found {
            match self
                .assets
                .iter_mut()
                .find(|asset| &asset.original_path == path)
            {
                None => self.import(&library.id, path),
                Some(asset) if asset.library_id != library.id => {}
                Some(asset) if asset.is_offline => {
                    asset.is_offline = false;
                    asset.is_trashed = false;
                    asset.updated_at_ms = now;
                }
                Some(_) => {}
            }
        }
        for asset in &mut self.assets {
            let looked = {
                let roots: Vec<&String> = library
                    .import_paths
                    .iter()
                    .filter(|root| is_under(&asset.original_path, root))
                    .collect();
                roots.is_empty() || roots.iter().any(|root| scanned.contains(*root))
            };
            if asset.library_id == library.id
                && !asset.is_offline
                && !found.contains(&asset.original_path)
                && looked
            {
                asset.is_offline = true;
                asset.is_trashed = true;
                asset.updated_at_ms = now;
            }
        }
    }

    fn import(&mut self, library_id: &str, path: &str) {
        let asset = Asset {
            id: new_id(),
            library_id: library_id.to_owned(),
            original_path: path.to_owned(),
            is_offline: false,
            is_trashed: false,
            visibility: "timeline".to_owned(),
            updated_at_ms: self.search.clock_ms,
            is_favorite: false,
            rating: None,
            description: None,
            tags: Vec::new(),
            has_metadata: !self.world.slow_imports,
            seq: next_number(),
        };
        self.assets.push(asset);
    }
}

fn tag_json(tag: &Tag) -> Value {
    json!({
        "id": tag.id,
        "name": tag.name,
        "value": tag.value,
        "createdAt": TIMESTAMP,
        "updatedAt": TIMESTAMP,
    })
}

fn is_media(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        MEDIA
            .split(' ')
            .any(|known| known.eq_ignore_ascii_case(extension))
    })
}

/// Walks like Immich's crawler: hidden entries, files that are not media and what the exclusion
/// patterns match are skipped.
fn walk(real_dir: &Path, immich_dir: &str, exclusions: &[Vec<Part>], out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(real_dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let immich_path = format!("{}/{name}", if immich_dir == "/" { "" } else { immich_dir });
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if !excluded(exclusions, &immich_path) {
                walk(&entry.path(), &immich_path, exclusions, out);
            }
        } else if is_media(&name)
            && !excluded(exclusions, &immich_path)
            && (kind.is_file() || entry.path().is_file())
        {
            out.insert(immich_path);
        }
    }
}

// ----------------------------------------------------------------------------
// The glob dialect of the exclusion patterns
// ----------------------------------------------------------------------------

/// A pattern as `tools/mock-immich/src/glob.ts` reads it. A pattern is matched against the whole
/// path as Immich sees it; everything the parts do not name is literal.
#[derive(Debug)]
enum Part {
    Literal(char),
    /// `?`: one character but `/`.
    One,
    /// `*`: anything but `/`.
    Segment,
    /// `**` anywhere else: anything.
    Any,
    /// `**/` at the start of a segment: any number of directories, also none.
    Dirs,
    /// `/**` at the end: the folder itself and everything below it.
    Below,
}

fn compile(pattern: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut index = 0;
    while index < pattern.len() {
        let rest = &pattern[index..];
        if rest == "/**" {
            parts.push(Part::Below);
            break;
        }
        let segment_start = index == 0 || pattern[..index].ends_with('/');
        if segment_start && rest.starts_with("**/") {
            parts.push(Part::Dirs);
            index += 3;
        } else if rest.starts_with("**") {
            parts.push(Part::Any);
            index += 2;
        } else if let Some(next) = rest.chars().next() {
            parts.push(match next {
                '*' => Part::Segment,
                '?' => Part::One,
                other => Part::Literal(other),
            });
            index += next.len_utf8();
        } else {
            break;
        }
    }
    parts
}

fn is_match(parts: &[Part], text: &[char]) -> bool {
    let Some((part, rest)) = parts.split_first() else {
        return text.is_empty();
    };
    match part {
        Part::Literal(wanted) => text.first() == Some(wanted) && is_match(rest, &text[1..]),
        Part::One => text.first().is_some_and(|next| *next != '/') && is_match(rest, &text[1..]),
        Part::Segment => {
            let run = text.iter().take_while(|next| **next != '/').count();
            (0..=run).any(|taken| is_match(rest, &text[taken..]))
        }
        Part::Any => (0..=text.len()).any(|taken| is_match(rest, &text[taken..])),
        Part::Dirs => {
            is_match(rest, text)
                || text
                    .iter()
                    .enumerate()
                    .any(|(at, next)| *next == '/' && is_match(rest, &text[at + 1..]))
        }
        Part::Below => text.is_empty() || text.first() == Some(&'/'),
    }
}

/// Whether a file or a directory (`path` without a trailing slash) is excluded.
fn excluded(patterns: &[Vec<Part>], path: &str) -> bool {
    let text: Vec<char> = path.chars().collect();
    patterns.iter().any(|parts| is_match(parts, &text))
}

// ----------------------------------------------------------------------------
// What a test controls
// ----------------------------------------------------------------------------

impl FakeImmich {
    /// Makes the real directory `dir` the content behind the path `prefix` of the Immich server
    /// (`/mnt/photos`): what a library scan reads, and where a forced delete of an online asset
    /// does its damage.
    pub fn mount(&self, prefix: &str, dir: &Path) {
        assert!(
            prefix.starts_with('/'),
            "a mount prefix is an absolute path"
        );
        let prefix = trim_slash(prefix).to_owned();
        let mut state = self.state();
        state.world.mounts.retain(|(known, _)| known != &prefix);
        state.world.mounts.push((prefix, dir.to_path_buf()));
    }

    /// Runs the scan of a library now, as Immich does when its own job runs. Not recorded as a
    /// call.
    pub fn scan_library(&self, library_id: &str) {
        self.state().scan(library_id);
    }

    /// [`FakeImmich::scan_library`] for every library.
    pub fn scan_all(&self) {
        let mut state = self.state();
        let ids: Vec<String> = state
            .libraries
            .iter()
            .map(|library| library.id.clone())
            .collect();
        for id in ids {
            state.scan(&id);
        }
    }

    /// With `hold`, `POST /libraries/{id}/scan` answers 204 and records the request, but the scan
    /// only runs at [`FakeImmich::run_held_scans`]: Immich is slow, or never gets to it.
    pub fn hold_scans(&self, hold: bool) {
        self.state().world.hold_scans = hold;
    }

    /// Runs the scans that were asked for while they were held; how many ran.
    pub fn run_held_scans(&self) -> usize {
        let mut state = self.state();
        let held = std::mem::take(&mut state.world.held);
        for id in &held {
            state.scan(id);
        }
        held.len()
    }

    /// With `slow`, the assets a scan imports have no metadata yet (`hasMetadata: false`) until
    /// [`FakeImmich::finish_imports`].
    pub fn slow_imports(&self, slow: bool) {
        self.state().world.slow_imports = slow;
    }

    /// Immich has read the metadata of every asset.
    pub fn finish_imports(&self) {
        let mut state = self.state();
        let now = state.search.clock_ms;
        for asset in &mut state.assets {
            if !asset.has_metadata {
                asset.has_metadata = true;
                asset.updated_at_ms = now;
            }
        }
    }

    /// The status `GET /assets/{id}` answers for an asset it does not know: 400 (the default) or 404.
    pub fn unknown_asset_status(&self, status: u16) {
        assert!(matches!(status, 400 | 404), "Immich answers 400 or 404");
        self.state().world.unknown_asset_status = status;
    }

    pub fn exclude(&self, library_id: &str, pattern: &str) {
        let mut state = self.state();
        let library = state
            .libraries
            .iter_mut()
            .find(|library| library.id == library_id)
            .expect("the fake Immich has no such library");
        library.exclusion_patterns.push(pattern.to_owned());
    }

    pub fn add_album(&self, name: &str, asset_ids: &[&str]) -> String {
        let album = Album {
            id: new_id(),
            name: name.to_owned(),
            asset_ids: asset_ids.iter().map(|id| (*id).to_owned()).collect(),
        };
        let id = album.id.clone();
        self.state().world.albums.push(album);
        id
    }

    pub fn add_tag(&self, value: &str) -> String {
        let tag = Tag {
            id: new_id(),
            name: value.rsplit('/').next().unwrap_or(value).to_owned(),
            value: value.to_owned(),
        };
        let id = tag.id.clone();
        self.state().world.tags.push(tag);
        id
    }

    pub fn tag_asset(&self, tag_id: &str, asset_id: &str) {
        let mut state = self.state();
        let asset = state
            .assets
            .iter_mut()
            .find(|asset| asset.id == asset_id)
            .expect("the fake Immich has no such asset");
        if !asset.tags.iter().any(|held| held == tag_id) {
            asset.tags.push(tag_id.to_owned());
        }
    }

    /// The names of the albums that hold the asset, sorted.
    pub fn albums_of(&self, asset_id: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .state()
            .world
            .albums
            .iter()
            .filter(|album| album.asset_ids.iter().any(|held| held == asset_id))
            .map(|album| album.name.clone())
            .collect();
        names.sort();
        names
    }

    /// The tags of the asset by their whole path, sorted.
    pub fn tags_of(&self, asset_id: &str) -> Vec<String> {
        let state = self.state();
        let mut values: Vec<String> = state
            .assets
            .iter()
            .find(|asset| asset.id == asset_id)
            .map(|asset| {
                asset
                    .tags
                    .iter()
                    .filter_map(|id| state.world.tags.iter().find(|tag| &tag.id == id))
                    .map(|tag| tag.value.clone())
                    .collect()
            })
            .unwrap_or_default();
        values.sort();
        values
    }

    pub fn has_asset(&self, id: &str) -> bool {
        self.state().assets.iter().any(|asset| asset.id == id)
    }

    /// Every asset (any state) Immich holds for `original_path`.
    pub fn assets_at(&self, original_path: &str) -> Vec<Asset> {
        self.state()
            .assets
            .iter()
            .filter(|asset| asset.original_path == original_path)
            .cloned()
            .collect()
    }

    /// The one asset Immich holds for `original_path`, which must exist and be the only one.
    pub fn asset_at(&self, original_path: &str) -> Asset {
        let mut assets = self.assets_at(original_path);
        assert_eq!(assets.len(), 1, "assets at {original_path}: {assets:?}");
        assets.remove(0)
    }

    pub fn violations(&self) -> Vec<Violation> {
        self.state().world.violations.clone()
    }

    /// The forced deletes of online assets, which must never happen.
    pub fn assert_no_violations(&self) {
        let violations = self.violations();
        assert!(
            violations.is_empty(),
            "the application had Immich delete an asset that was online: {violations:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, path: &str) -> bool {
        excluded(&[compile(pattern)], path)
    }

    #[test]
    fn a_double_star_matches_any_number_of_directories() {
        assert!(matches("**/@eaDir/**", "/mnt/photo/2019/@eaDir/x.jpg"));
        assert!(matches("**/@eaDir/**", "/mnt/photo/@eaDir"));
        assert!(matches("**/@eaDir/**", "@eaDir/x.jpg"));
        assert!(!matches("**/@eaDir/**", "/mnt/photo/2019/x.jpg"));
        assert!(!matches("**/@eaDir/**", "/mnt/photo/@eaDirx/x.jpg"));
    }

    #[test]
    fn a_star_stays_inside_one_segment_and_a_question_mark_is_one_character() {
        assert!(matches("/mnt/*/x.jpg", "/mnt/photo/x.jpg"));
        assert!(!matches("/mnt/*/x.jpg", "/mnt/photo/2019/x.jpg"));
        assert!(matches("/mnt/photo/IMG_?.jpg", "/mnt/photo/IMG_1.jpg"));
        assert!(!matches("/mnt/photo/IMG_?.jpg", "/mnt/photo/IMG_12.jpg"));
        assert!(!matches("/mnt/photo/IMG_?.jpg", "/mnt/photo/IMG_/.jpg"));
    }

    #[test]
    fn a_trailing_double_star_takes_the_folder_and_what_is_below() {
        assert!(matches("/mnt/photo/skip/**", "/mnt/photo/skip"));
        assert!(matches("/mnt/photo/skip/**", "/mnt/photo/skip/a/b.jpg"));
        assert!(!matches("/mnt/photo/skip/**", "/mnt/photo/skipped/a.jpg"));
    }

    #[test]
    fn a_mount_covers_the_paths_below_its_prefix_only() {
        let mut world = World::default();
        world
            .mounts
            .push(("/mnt/photos".to_owned(), PathBuf::from("/real/photos")));
        assert_eq!(
            world.real_path("/mnt/photos/2019/a.jpg"),
            Some(PathBuf::from("/real/photos/2019/a.jpg"))
        );
        assert_eq!(
            world.real_path("/mnt/photos"),
            Some(PathBuf::from("/real/photos"))
        );
        assert_eq!(world.real_path("/mnt/photos2/a.jpg"), None);
        assert_eq!(world.real_path("/mnt/photos/../x"), None);
    }
}
