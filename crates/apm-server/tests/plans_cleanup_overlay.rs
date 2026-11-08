//! SQL child grouping must agree with exact-byte Rust grouping, not normalized names.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use apm_core::dto::{CreatePlanRequest, Filter, PlanKind, PlanRef, PlanSide, SortKey, SortOrder};
use apm_core::paths::RelPath;
use apm_server::db::subtree::Subtree;
use apm_server::plans::overlay::{GhostView, PlanOverlay};
use common::{TestApp, seed::Seed};
use serde_json::json;
use std::collections::HashMap;

#[tokio::test]
async fn grouped_counts_match_rust_for_adversarial_children_at_root_and_unicode_base() {
    let app = TestApp::new().await;
    let (v, _) = app.add_volume("photos").await;
    let seed = Seed::new(&app.state.db);
    let names = [
        "a",
        "a.b",
        "a b",
        "a_b",
        "a!x",
        "A",
        "%",
        "_",
        "\\",
        "été",
        "e\u{301}te\u{301}",
        "日本",
    ];
    for base in ["", "scope/été"] {
        let rel = RelPath::new(base).unwrap();
        for name in names {
            let child = rel.join(name).unwrap();
            seed.dir(
                v,
                child
                    .join_rel(&RelPath::new("deep/next/end").unwrap())
                    .as_str(),
            )
            .await;
        }
        // A parent outside the removal set must not acquire a flag just because a child is empty.
        seed.file(v, rel.join("full").unwrap().as_str(), "orphan.xmp")
            .insert()
            .await;
        seed.dir(v, rel.join("full").unwrap().join("empty").unwrap().as_str())
            .await;
    }
    let plan = app
        .state
        .plans
        .create(CreatePlanRequest {
            kind: PlanKind::Cleanup,
            params: json!({"scope":{"volume_id":v,"dir":"","recursive":true}}),
            replaces: None,
        })
        .await
        .unwrap();
    let paths: Vec<String> = sqlx::query_scalar("SELECT path FROM cleanup_dirs WHERE plan_id = ?")
        .bind(plan.id)
        .fetch_all(app.state.db.read())
        .await
        .unwrap();
    for base in ["", "scope/été"] {
        let dir = RelPath::new(base).unwrap();
        let subtree = Subtree::new(&dir);
        let children: Vec<_> = names
            .into_iter()
            .chain(["full", "absent"])
            .map(str::to_owned)
            .collect();
        let mut expected = HashMap::<String, i64>::new();
        for path in paths
            .iter()
            .filter(|path| subtree.contains(path) && path.as_str() != base)
        {
            let rest = if base.is_empty() {
                path.as_str()
            } else {
                path.strip_prefix(base).unwrap().strip_prefix('/').unwrap()
            };
            let name = rest.split('/').next().unwrap();
            if paths.contains(&dir.join(name).unwrap().as_str().to_owned())
                && children.iter().any(|c| c == name)
            {
                *expected.entry(name.to_owned()).or_default() += 1;
            }
        }
        let view = GhostView {
            volume_id: v,
            dir,
            filter: Filter::default(),
            sort: SortKey::Name,
            order: SortOrder::Asc,
        };
        let overlay = app
            .state
            .plans
            .folder_overlay(
                PlanRef {
                    plan_id: plan.id,
                    side: PlanSide::Comparison,
                },
                &view,
                &children,
            )
            .await
            .unwrap();
        let actual: HashMap<_, _> = overlay
            .per_child
            .into_iter()
            .map(|(name, info)| {
                assert!(info.overlay.will_delete);
                (name, info.matches.unwrap())
            })
            .collect();
        assert_eq!(actual, expected, "{base:?}");
        assert_ne!(actual.get("a"), None);
        assert_ne!(actual.get("A"), None);
        assert!(actual.contains_key("été") && actual.contains_key("e\u{301}te\u{301}"));
    }
}
