use super::*;
use crate::db::{
    Db,
    testing::{insert_file, insert_volume},
};
use opm_core::dto::{Filter, Scope};
use opm_core::hash::{Sha1Digest, sha1_bytes};
use opm_core::matching::{MatchFacts, MatchOptions, is_match};

struct Fact {
    id: i64,
    name: String,
    dir: String,
    hash: Option<Sha1Digest>,
    time: Option<i64>,
    rating: u8,
}

impl Fact {
    fn facts(&self) -> MatchFacts<'_> {
        MatchFacts {
            id: self.id,
            name: &self.name,
            hash: self.hash,
            capture_ts: self.time,
            size: 1,
        }
    }
    fn in_side(&self, side: &SideParams) -> bool {
        let dir = &side.scope.dir;
        (dir.is_empty()
            || self.dir == *dir
            || side.scope.recursive && self.dir.starts_with(&format!("{dir}/")))
            && (side.filter.ratings.is_empty() || side.filter.ratings.contains(&self.rating))
    }
}

#[derive(Default)]
struct Collect(HashSet<Pair>);
impl PairVisitor for Collect {
    fn visit(&mut self, pair: Pair) -> BoxFuture<'_, Result<(), PlanError>> {
        self.0.insert(pair);
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn random_collisions_filters_and_overlaps_match_reference_under_both_policies() {
    let temp = Db::open_temp().await.unwrap();
    let db = temp.handle();
    let volume = insert_volume(&db, "v").await;
    let names = [
        "one.jpg",
        "ONE.JPG",
        "one2.jpg",
        "one_edit.CR3",
        "\u{e9}.jpg",
        "e\u{301}.jpg",
    ];
    let mut random = 0x12345678_u64;
    let mut next = || {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        random
    };
    let mut facts = Vec::new();
    for index in 0..36 {
        let name = names[(next() % names.len() as u64) as usize].to_owned();
        let dir = format!("set{}/leaf{index}", next() % 3);
        let id = insert_file(&db, volume, &dir, &name).await;
        let hash = match next() % 3 {
            0 => None,
            n => Some(sha1_bytes(&[n as u8])),
        };
        let time = match next() % 3 {
            0 => None,
            n => Some(n as i64),
        };
        let rating = (next() % 3) as u8;
        sqlx::query(
            "UPDATE files SET kind = 'image', hash = ?, capture_ts = ?, rating = ? WHERE id = ?",
        )
        .bind(hash.map(|h| h.to_vec()))
        .bind(time)
        .bind(rating)
        .bind(id)
        .execute(db.write())
        .await
        .unwrap();
        facts.push(Fact {
            id,
            name,
            dir,
            hash,
            time,
            rating,
        });
    }
    let cancel = CancellationToken::new();
    for iteration in 0..16 {
        let side = |dir: &str, rating: Option<u8>| SideParams {
            scope: Scope {
                volume_id: Some(volume),
                dir: dir.to_owned(),
                recursive: true,
            },
            filter: Filter {
                ratings: rating.into_iter().collect(),
                ..Filter::default()
            },
        };
        let source = side(
            if iteration % 3 == 0 { "" } else { "set0" },
            if iteration % 2 == 0 {
                Some((next() % 3) as u8)
            } else {
                None
            },
        );
        let comparison = side(
            if iteration % 4 == 0 {
                ""
            } else if iteration % 2 == 0 {
                "set1"
            } else {
                "set0"
            },
            if iteration % 5 == 0 {
                Some((next() % 3) as u8)
            } else {
                None
            },
        );
        for mode in MatchMode::ALL {
            for strict_prefix in [false, true] {
                for policy in [Policy::Oriented, Policy::Unoriented] {
                    let params = DuplicateParams {
                        mode,
                        strict_prefix,
                        source: source.clone(),
                        comparison: comparison.clone(),
                    };
                    let mut expected = HashSet::new();
                    for s in facts.iter().filter(|f| f.in_side(&source)) {
                        for c in facts.iter().filter(|f| f.in_side(&comparison)) {
                            let pair = Pair::new(s.id, c.id);
                            if is_match(
                                mode,
                                &s.facts(),
                                &c.facts(),
                                MatchOptions { strict_prefix },
                            ) && (policy == Policy::Unoriented
                                || is_valid_pair(
                                    pair,
                                    Overlap {
                                        source_in_comparison: s.in_side(&comparison),
                                        comparison_in_source: c.in_side(&source),
                                    },
                                ))
                            {
                                expected.insert(pair);
                            }
                        }
                    }
                    let mut collected = Collect::default();
                    let mut conn = db.read().acquire().await.unwrap();
                    candidate_pairs(&mut conn, &params, policy, &cancel, &mut collected, &[])
                        .await
                        .unwrap();
                    assert_eq!(
                        collected.0, expected,
                        "{iteration} {mode:?} {strict_prefix} {policy:?}"
                    );
                }
            }
        }
    }
}
