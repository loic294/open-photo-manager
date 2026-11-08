//! Carrying what the user set on an Immich asset over to the asset that replaces it.
//!
//! Immich keeps favorites, ratings, descriptions, albums and tags on the asset record, and a moved
//! file becomes a NEW asset while the old record turns offline, so the user's work would vanish with
//! the old record. The pass therefore
//!
//! 1. takes a [`Snapshot`] of the old asset while it is still live (Immich may hide offline or
//!    trashed assets from some endpoints, so nothing is ever read from them later) and keeps it in
//!    the row, and
//! 2. once the new asset has its metadata (`hasMetadata`; Immich's import job would overwrite a
//!    rating or a description written earlier), computes a [`Plan`] against the FRESH new asset and
//!    [`apply`]s it.
//!
//! Carrying is best effort and NON-DESTRUCTIVE: a field is only set when the new asset still has its
//! default and the old value means something (favorite, 1 to 5 stars, a description, `archive`;
//! never `hidden` or `locked`), and tags and albums are only added. Every part is idempotent and the
//! plan is recomputed on every attempt, so repeating a half-done carry-over never does harm.
//!
//! What is NOT carried: people, stacks, shared links, the position in an album and the sharing of
//! an album. An album or a tag that no longer exists is skipped.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::immich::client::ImmichClient;
use crate::immich::error::{ImmichError, truncate_chars};
use crate::immich::models::{AssetUpdate, AssetVisibility, ImmichAlbum, ImmichAsset};

/// The longest part of an Immich reason kept in a message.
const REASON_CHARS: usize = 60;

/// What the old asset had when the pass looked at it, as stored in `snapshot_json`. Every field has
/// a default so a snapshot written by another version still reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Snapshot {
    favorite: bool,
    /// 1 to 5, 0 for none.
    rating: u8,
    description: Option<String>,
    archived: bool,
    tag_ids: Vec<String>,
    album_ids: Vec<String>,
}

impl Snapshot {
    /// The snapshot of `asset`, which must come from `GET /assets/{id}` (a search page leaves the
    /// tags and the exif data out), with the albums `GET /albums?assetId=` reported.
    pub(super) fn of(asset: &ImmichAsset, albums: &[ImmichAlbum]) -> Self {
        Self {
            favorite: asset.is_favorite,
            rating: asset.rating(),
            description: asset.description().map(str::to_owned),
            archived: asset.visibility == AssetVisibility::Archive,
            tag_ids: unique(asset.tags.iter().map(|tag| tag.id.as_str())),
            album_ids: unique(albums.iter().map(|album| album.id.as_str())),
        }
    }

    pub(super) fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_owned())
    }

    /// `None` for text that is not a snapshot.
    pub(super) fn from_json(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }

    /// What has to be added to `new` (the fresh asset that replaces the one of the snapshot).
    pub(super) fn plan(&self, new: &ImmichAsset) -> Plan {
        let mut update = AssetUpdate::default();
        if self.favorite && !new.is_favorite {
            update.is_favorite = Some(true);
        }
        if (1..=5).contains(&self.rating) && new.rating() == 0 {
            update.rating = Some(self.rating);
        }
        if self.archived && new.visibility == AssetVisibility::Timeline {
            update.visibility = Some(AssetVisibility::Archive);
        }
        if let Some(text) = self
            .description
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            && new.description().is_none()
        {
            update.description = Some(text.to_owned());
        }
        let tagged: HashSet<&str> = new.tags.iter().map(|tag| tag.id.as_str()).collect();
        Plan {
            update,
            album_ids: self.album_ids.clone(),
            tag_ids: self
                .tag_ids
                .iter()
                .filter(|id| !tagged.contains(id.as_str()))
                .cloned()
                .collect(),
        }
    }
}

/// The ids in order, without repeats and without empty ones.
fn unique<'a>(ids: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.filter(|id| !id.is_empty() && seen.insert(*id))
        .map(str::to_owned)
        .collect()
}

/// The calls that carry a [`Snapshot`] over to one new asset.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Plan {
    /// The fields to set on the asset (`PUT /assets`).
    pub(super) update: AssetUpdate,
    /// Every album of the old asset. An asset that is already in one counts as done.
    pub(super) album_ids: Vec<String>,
    /// The tags the new asset does not have yet.
    pub(super) tag_ids: Vec<String>,
}

impl Plan {
    pub(super) fn is_empty(&self) -> bool {
        self.update.is_empty() && self.album_ids.is_empty() && self.tag_ids.is_empty()
    }
}

/// Why a carry-over stopped.
#[derive(Debug)]
pub(super) enum CarryError {
    /// A call failed; the error says whether trying again can help.
    Immich(ImmichError),
    /// Immich answered but refused an asset or an album; asking again will not change that. The
    /// text is fit for a person and holds no file name.
    Refused(String),
}

impl From<ImmichError> for CarryError {
    fn from(err: ImmichError) -> Self {
        Self::Immich(err)
    }
}

/// Carries `plan` over to the asset `asset_id`: the fields first, then the albums, then the tags.
/// Stops at the first failure; the parts that were done stay done and are done again harmlessly.
pub(super) async fn apply(
    client: &ImmichClient,
    asset_id: &str,
    plan: &Plan,
) -> Result<(), CarryError> {
    let ids = [asset_id.to_owned()];
    client.update_assets(&ids, &plan.update).await?;
    for album_id in &plan.album_ids {
        add_to_album(client, album_id, &ids).await?;
    }
    add_tags(client, &plan.tag_ids, &ids).await
}

async fn add_to_album(
    client: &ImmichClient,
    album_id: &str,
    ids: &[String],
) -> Result<(), CarryError> {
    match client.add_assets_to_album(album_id, ids).await {
        Ok(results) => match results.iter().find(|result| !result.is_ok()) {
            None => Ok(()),
            Some(refused) => Err(CarryError::Refused(format!(
                "Immich would not add the file to an album ({})",
                truncate_chars(refused.error.as_deref().unwrap_or("unknown"), REASON_CHARS)
            ))),
        },
        // The album was deleted (or the account lost it) since the snapshot: nothing to restore.
        Err(err) if is_gone(&err) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

async fn add_tags(
    client: &ImmichClient,
    tag_ids: &[String],
    ids: &[String],
) -> Result<(), CarryError> {
    if tag_ids.is_empty() {
        return Ok(());
    }
    match client.tag_assets(tag_ids, ids).await {
        Ok(_) => Ok(()),
        Err(err) if is_gone(&err) => {
            // Immich names no culprit, so the list of tags decides: keep the ones that still exist.
            let known: HashSet<String> =
                client.tags().await?.into_iter().map(|tag| tag.id).collect();
            let remaining: Vec<String> = tag_ids
                .iter()
                .filter(|id| known.contains(*id))
                .cloned()
                .collect();
            if remaining.len() == tag_ids.len() {
                return Err(err.into());
            }
            client.tag_assets(&remaining, ids).await?;
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

/// Immich does not know the item: a 404, or the 400 "Not found or no ... access" it uses instead.
pub(super) fn is_gone(err: &ImmichError) -> bool {
    match err {
        ImmichError::NotFound => true,
        ImmichError::Upstream {
            status: 400 | 404,
            message,
        } => message.to_ascii_lowercase().contains("not found"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::{Value, json};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::immich::client::ClientOptions;
    use crate::security::Redacted;

    const NEW: &str = "00000000-0000-4000-8000-0000000000aa";
    const ALBUM_A: &str = "00000000-0000-4000-8000-0000000000a1";
    const ALBUM_B: &str = "00000000-0000-4000-8000-0000000000a2";
    const TAG_A: &str = "00000000-0000-4000-8000-0000000000b1";
    const TAG_B: &str = "00000000-0000-4000-8000-0000000000b2";

    fn asset(extra: Value) -> ImmichAsset {
        let mut body = json!({
            "id": "old",
            "originalPath": "/mnt/photos/a.jpg",
            "isFavorite": false,
            "visibility": "timeline",
            "tags": [],
        });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        serde_json::from_value(body).unwrap()
    }

    fn album(id: &str) -> ImmichAlbum {
        ImmichAlbum {
            id: id.to_owned(),
            album_name: "Album".to_owned(),
        }
    }

    fn everything() -> Snapshot {
        let old = asset(json!({
            "isFavorite": true,
            "visibility": "archive",
            "exifInfo": { "rating": 4, "description": "Summer trip" },
            "tags": [{ "id": TAG_A, "name": "a", "value": "a" }, { "id": TAG_B, "name": "b", "value": "b" }],
        }));
        Snapshot::of(&old, &[album(ALBUM_A), album(ALBUM_B)])
    }

    // ----- the snapshot ----------------------------------------------------------------------

    #[test]
    fn a_snapshot_keeps_what_the_user_set() {
        let snapshot = everything();
        assert!(snapshot.favorite);
        assert_eq!(snapshot.rating, 4);
        assert_eq!(snapshot.description.as_deref(), Some("Summer trip"));
        assert!(snapshot.archived);
        assert_eq!(snapshot.tag_ids, [TAG_A, TAG_B]);
        assert_eq!(snapshot.album_ids, [ALBUM_A, ALBUM_B]);
    }

    #[test]
    fn a_plain_asset_gives_a_snapshot_with_nothing_to_carry() {
        let snapshot = Snapshot::of(&asset(json!({})), &[]);
        assert_eq!(snapshot, Snapshot::default());
        assert!(snapshot.plan(&asset(json!({}))).is_empty());
    }

    #[test]
    fn a_rejected_rating_and_a_blank_description_count_as_nothing() {
        let old = asset(json!({ "exifInfo": { "rating": -1, "description": "  \n" } }));
        let snapshot = Snapshot::of(&old, &[]);
        assert_eq!(snapshot.rating, 0);
        assert_eq!(snapshot.description, None);
    }

    #[test]
    fn repeated_and_empty_ids_are_dropped_in_order() {
        let old = asset(json!({
            "tags": [
                { "id": TAG_B, "name": "b", "value": "b" },
                { "id": TAG_B, "name": "b", "value": "b" },
                { "id": "", "name": "x", "value": "x" },
                { "id": TAG_A, "name": "a", "value": "a" },
            ],
        }));
        let snapshot = Snapshot::of(&old, &[album(ALBUM_B), album(ALBUM_A), album(ALBUM_B)]);
        assert_eq!(snapshot.tag_ids, [TAG_B, TAG_A]);
        assert_eq!(snapshot.album_ids, [ALBUM_B, ALBUM_A]);
    }

    #[test]
    fn a_snapshot_survives_its_json_and_reads_old_or_partial_text() {
        let snapshot = everything();
        assert_eq!(Snapshot::from_json(&snapshot.to_json()), Some(snapshot));
        assert_eq!(Snapshot::from_json("{}"), Some(Snapshot::default()));
        assert_eq!(
            Snapshot::from_json(r#"{"favorite":true,"someday":[1,2]}"#),
            Some(Snapshot {
                favorite: true,
                ..Snapshot::default()
            })
        );
        assert_eq!(Snapshot::from_json("not json"), None);
        assert_eq!(Snapshot::from_json(r#"{"favorite":"yes"}"#), None);
    }

    // ----- the plan --------------------------------------------------------------------------

    #[test]
    fn everything_is_carried_to_a_plain_new_asset() {
        let plan = everything().plan(&asset(json!({})));
        assert_eq!(plan.update.is_favorite, Some(true));
        assert_eq!(plan.update.rating, Some(4));
        assert_eq!(plan.update.visibility, Some(AssetVisibility::Archive));
        assert_eq!(plan.update.description.as_deref(), Some("Summer trip"));
        assert_eq!(plan.album_ids, [ALBUM_A, ALBUM_B]);
        assert_eq!(plan.tag_ids, [TAG_A, TAG_B]);
        assert!(!plan.is_empty());
    }

    #[test]
    fn a_field_the_new_asset_already_has_is_left_alone() {
        let new = asset(json!({
            "isFavorite": true,
            "visibility": "archive",
            "exifInfo": { "rating": 2, "description": "From the file" },
            "tags": [{ "id": TAG_A, "name": "a", "value": "a" }],
        }));
        let plan = everything().plan(&new);
        assert_eq!(plan.update, AssetUpdate::default());
        assert_eq!(plan.tag_ids, [TAG_B]);
        assert_eq!(plan.album_ids, [ALBUM_A, ALBUM_B]);
    }

    #[test]
    fn visibility_is_only_archived_over_a_timeline_asset() {
        for (visibility, expected) in [
            ("timeline", Some(AssetVisibility::Archive)),
            ("archive", None),
            ("hidden", None),
            ("locked", None),
            ("somethingnew", None),
        ] {
            let plan = everything().plan(&asset(json!({ "visibility": visibility })));
            assert_eq!(plan.update.visibility, expected, "{visibility}");
        }
    }

    #[test]
    fn hidden_and_locked_old_assets_stay_unarchived() {
        for visibility in ["hidden", "locked", "timeline"] {
            let old = asset(json!({ "visibility": visibility }));
            let plan = Snapshot::of(&old, &[]).plan(&asset(json!({})));
            assert_eq!(plan.update.visibility, None, "{visibility}");
        }
    }

    #[test]
    fn a_new_asset_with_a_blank_description_still_gets_the_old_one() {
        let new = asset(json!({ "exifInfo": { "description": " " } }));
        assert_eq!(
            everything().plan(&new).update.description.as_deref(),
            Some("Summer trip")
        );
    }

    #[test]
    fn the_plan_never_unsets_anything() {
        let plain = Snapshot::of(&asset(json!({})), &[]);
        let new = asset(json!({
            "isFavorite": true,
            "visibility": "archive",
            "exifInfo": { "rating": 5, "description": "Mine" },
        }));
        assert!(plain.plan(&new).is_empty());
    }

    // ----- applying it -----------------------------------------------------------------------

    fn client(server: &MockServer) -> ImmichClient {
        ImmichClient::new(
            &server.uri(),
            Some(Redacted::new("apm-test-key".to_owned())),
            ClientOptions::default(),
        )
        .unwrap()
    }

    async fn calls(server: &MockServer) -> Vec<(String, String, Value)> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|request| {
                (
                    request.method.to_string(),
                    request.url.path().to_owned(),
                    request.body_json().unwrap_or(Value::Null),
                )
            })
            .collect()
    }

    async fn answer(server: &MockServer, verb: &str, route: &str, template: ResponseTemplate) {
        Mock::given(method(verb))
            .and(path(route))
            .respond_with(template)
            .mount(server)
            .await;
    }

    fn error(status: u16, message: &str) -> ResponseTemplate {
        ResponseTemplate::new(status).set_body_json(json!({ "message": message }))
    }

    fn added() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!([{ "id": NEW, "success": true }]))
    }

    #[tokio::test]
    async fn applying_sends_the_fields_then_each_album_then_the_tags() {
        let server = MockServer::start().await;
        answer(&server, "PUT", "/api/assets", ResponseTemplate::new(204)).await;
        answer(
            &server,
            "PUT",
            &format!("/api/albums/{ALBUM_A}/assets"),
            added(),
        )
        .await;
        answer(
            &server,
            "PUT",
            &format!("/api/albums/{ALBUM_B}/assets"),
            ResponseTemplate::new(200)
                .set_body_json(json!([{ "id": NEW, "success": false, "error": "duplicate" }])),
        )
        .await;
        answer(
            &server,
            "PUT",
            "/api/tags/assets",
            ResponseTemplate::new(200).set_body_json(json!({ "count": 1 })),
        )
        .await;

        let plan = everything().plan(&asset(json!({})));
        apply(&client(&server), NEW, &plan).await.unwrap();

        let sent = calls(&server).await;
        let order: Vec<&str> = sent.iter().map(|call| call.1.as_str()).collect();
        assert_eq!(
            order,
            [
                "/api/assets".to_owned(),
                format!("/api/albums/{ALBUM_A}/assets"),
                format!("/api/albums/{ALBUM_B}/assets"),
                "/api/tags/assets".to_owned(),
            ]
        );
        assert_eq!(
            sent[0].2,
            json!({
                "ids": [NEW],
                "isFavorite": true,
                "rating": 4,
                "visibility": "archive",
                "description": "Summer trip",
            })
        );
        assert_eq!(sent[1].2, json!({ "ids": [NEW] }));
        assert_eq!(
            sent[3].2,
            json!({ "assetIds": [NEW], "tagIds": [TAG_A, TAG_B] })
        );
    }

    #[tokio::test]
    async fn an_empty_plan_sends_nothing() {
        let server = MockServer::start().await;
        let plan = Snapshot::default().plan(&asset(json!({})));
        apply(&client(&server), NEW, &plan).await.unwrap();
        assert!(calls(&server).await.is_empty());
    }

    #[tokio::test]
    async fn only_the_fields_that_changed_are_sent() {
        let server = MockServer::start().await;
        answer(&server, "PUT", "/api/assets", ResponseTemplate::new(204)).await;
        let snapshot = Snapshot {
            favorite: true,
            ..Snapshot::default()
        };
        let plan = snapshot.plan(&asset(json!({})));
        apply(&client(&server), NEW, &plan).await.unwrap();
        let sent = calls(&server).await;
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].2, json!({ "ids": [NEW], "isFavorite": true }));
    }

    #[tokio::test]
    async fn an_album_that_is_gone_is_skipped_and_the_others_still_get_the_asset() {
        let server = MockServer::start().await;
        answer(
            &server,
            "PUT",
            &format!("/api/albums/{ALBUM_A}/assets"),
            error(400, "Not found or no album.addAsset access"),
        )
        .await;
        answer(
            &server,
            "PUT",
            &format!("/api/albums/{ALBUM_B}/assets"),
            added(),
        )
        .await;
        let snapshot = Snapshot {
            album_ids: vec![ALBUM_A.to_owned(), ALBUM_B.to_owned()],
            ..Snapshot::default()
        };
        apply(&client(&server), NEW, &snapshot.plan(&asset(json!({}))))
            .await
            .unwrap();
        assert_eq!(calls(&server).await.len(), 2);
    }

    #[tokio::test]
    async fn an_album_that_refuses_the_asset_stops_the_carry_over() {
        let server = MockServer::start().await;
        answer(
            &server,
            "PUT",
            &format!("/api/albums/{ALBUM_A}/assets"),
            ResponseTemplate::new(200)
                .set_body_json(json!([{ "id": NEW, "success": false, "error": "no_permission" }])),
        )
        .await;
        let snapshot = Snapshot {
            album_ids: vec![ALBUM_A.to_owned(), ALBUM_B.to_owned()],
            tag_ids: vec![TAG_A.to_owned()],
            ..Snapshot::default()
        };
        let err = apply(&client(&server), NEW, &snapshot.plan(&asset(json!({}))))
            .await
            .unwrap_err();
        match err {
            CarryError::Refused(text) => assert!(text.contains("no_permission"), "{text}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(calls(&server).await.len(), 1, "nothing after the refusal");
    }

    #[tokio::test]
    async fn a_failing_call_is_reported_as_an_immich_error() {
        let server = MockServer::start().await;
        answer(&server, "PUT", "/api/assets", error(500, "boom")).await;
        let snapshot = Snapshot {
            favorite: true,
            ..Snapshot::default()
        };
        let err = apply(&client(&server), NEW, &snapshot.plan(&asset(json!({}))))
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                CarryError::Immich(ImmichError::Upstream { status: 500, .. })
            ),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_tag_that_is_gone_is_dropped_and_the_others_are_attached() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/tags/assets"))
            .respond_with(|request: &wiremock::Request| {
                let body: Value = request.body_json().unwrap();
                if body["tagIds"].as_array().unwrap().len() == 2 {
                    error(400, "Not found or no tag.asset access")
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({ "count": 1 }))
                }
            })
            .mount(&server)
            .await;
        answer(
            &server,
            "GET",
            "/api/tags",
            ResponseTemplate::new(200).set_body_json(json!([
                { "id": TAG_B, "name": "b", "value": "b" },
            ])),
        )
        .await;
        let snapshot = Snapshot {
            tag_ids: vec![TAG_A.to_owned(), TAG_B.to_owned()],
            ..Snapshot::default()
        };
        apply(&client(&server), NEW, &snapshot.plan(&asset(json!({}))))
            .await
            .unwrap();
        let sent = calls(&server).await;
        assert_eq!(sent.len(), 3, "{sent:?}");
        assert_eq!(sent[2].2, json!({ "assetIds": [NEW], "tagIds": [TAG_B] }));
    }

    #[tokio::test]
    async fn a_missing_asset_is_not_mistaken_for_a_missing_tag() {
        let server = MockServer::start().await;
        answer(
            &server,
            "PUT",
            "/api/tags/assets",
            error(400, "Not found or no asset.update access"),
        )
        .await;
        answer(
            &server,
            "GET",
            "/api/tags",
            ResponseTemplate::new(200).set_body_json(json!([
                { "id": TAG_A, "name": "a", "value": "a" },
            ])),
        )
        .await;
        let snapshot = Snapshot {
            tag_ids: vec![TAG_A.to_owned()],
            ..Snapshot::default()
        };
        let err = apply(&client(&server), NEW, &snapshot.plan(&asset(json!({}))))
            .await
            .unwrap_err();
        assert!(matches!(err, CarryError::Immich(_)), "{err:?}");
    }

    #[test]
    fn only_not_found_answers_count_as_gone() {
        assert!(is_gone(&ImmichError::NotFound));
        assert!(is_gone(&ImmichError::Upstream {
            status: 400,
            message: "Not found or no album.addAsset access".into()
        }));
        assert!(!is_gone(&ImmichError::Upstream {
            status: 400,
            message: "ids must be UUIDs".into()
        }));
        assert!(!is_gone(&ImmichError::Upstream {
            status: 500,
            message: "not found somewhere".into()
        }));
        assert!(!is_gone(&ImmichError::Unauthorized));
    }
}
