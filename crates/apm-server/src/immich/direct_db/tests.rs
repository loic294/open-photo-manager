#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;

#[test]
fn official_posix_normalization_preserves_case_unicode_and_backslashes() {
    for (input, expected) in [
        ("/mnt/photo//a/../b.jpg", "/mnt/photo/b.jpg"),
        ("/mnt/photo/./a.jpg", "/mnt/photo/a.jpg"),
        ("/../../a.jpg", "/a.jpg"),
        ("/mnt/photo/folder/", "/mnt/photo/folder/"),
        ("/mnt/photo/É\\A.jpg", "/mnt/photo/É\\A.jpg"),
        ("/mnt/photo/E\u{301}.jpg", "/mnt/photo/E\u{301}.jpg"),
    ] {
        assert_eq!(normalized_path(input).unwrap(), expected);
    }

    assert_eq!(
        normalized_path("relative.jpg"),
        Err(DbMoveError::Destination)
    );
}

#[test]
fn checksums_match_independent_node_posix_crypto_vectors() {
    for (path, expected) in [
        (
            "/photos/old.jpg",
            "06816731f3181376c4332513a6a2ab7126f20f00",
        ),
        (
            "/photos/new.jpg",
            "a993cebecffee59ea314fbb06fd1fdd997def2a3",
        ),
        (
            "/mnt/photo//a/../É\\A.jpg",
            "8997cb8a4e04e84da6add72730aa15bba400ae19",
        ),
        (
            "/mnt/photo/E\u{301}.jpg",
            "410d869ccf3d8430e9fe4a075449876827d6696a",
        ),
    ] {
        assert_eq!(
            path_checksum(path).unwrap(),
            apm_core::hash::parse_hex(expected).unwrap()
        );
    }
}

#[test]
fn content_checksum_is_preserved_and_unknown_algorithms_refused() {
    let spec = MoveSpec {
        asset_id: String::new(),
        owner_id: String::new(),
        library_id: String::new(),
        old: "/photos/old.jpg".into(),
        new: "/photos/new.jpg".into(),
        new_name: "new.jpg".into(),
        sidecars: vec![],
    };
    let mut row = AssetRow {
        owner_id: String::new(),
        library_id: None,
        external: true,
        original_path: spec.old.clone(),
        original_name: "old.jpg".into(),
        update_id: String::new(),
        checksum: vec![7; 20],
        checksum_algorithm: "sha1".into(),
    };
    assert_eq!(destination_checksum(&row, &spec).unwrap(), row.checksum);
    row.checksum_algorithm = "sha1-path".into();
    assert_eq!(
        destination_checksum(&row, &spec).unwrap(),
        path_checksum(&spec.new).unwrap()
    );
    assert_ne!(path_checksum(&spec.old), path_checksum(&spec.new));
    row.checksum_algorithm = "sha256".into();
    assert_eq!(
        destination_checksum(&row, &spec),
        Err(DbMoveError::Checksum)
    );
}

#[test]
fn tls_configuration_fails_closed_without_revealing_credentials() {
    for url in [
        "postgres://localhost/db",
        "postgres://localhost/db?sslmode=require",
        "postgres://localhost/db?sslmode=verify-full&options=bad",
        "not a URL",
    ] {
        assert!(matches!(
            connection_options(url),
            Err(DbMoveError::Configuration)
        ));
    }
    assert!(connection_options("postgres://localhost/db?sslmode=verify-full").is_ok());
    assert!(
        DbMoveError::Unavailable
            .refusal()
            .starts_with("No file was moved.")
    );
    assert!(
        DbMoveError::Unavailable
            .queued_warning()
            .contains("NOT confirmed")
    );
}

#[test]
fn preflight_refusal_gives_item_scoped_retry_advice() {
    let refusal = DbMoveError::NotConfigured.refusal();
    assert!(refusal.starts_with("No file was moved."));
    assert!(refusal.contains("only to this item"));
    assert!(refusal.contains("restart APM"));
    assert!(refusal.contains("rebuild the plan and retry this item"));
    let queued = DbMoveError::NotConfigured.queued_warning();
    assert!(queued.contains("The file moved"));
    assert!(!queued.contains("No file was moved"));
}

#[test]
fn default_exclusions_and_synology_rules_are_conservative() {
    let rules = [
        "**/@eaDir/**",
        "**/._*",
        "**/#recycle/**",
        "**/#snapshot/**",
        "**/.stversions/**",
        "**/.stfolder/**",
        "**/.apm-trash/**",
    ]
    .map(String::from);
    for path in [
        "/photos/@eaDir/a.jpg",
        "/photos/@EADIR/a.jpg",
        "/photos/#snapshot/a.jpg",
        "/photos/._a.jpg",
        "/photos/.apm-trash/a.jpg",
        "/photos/.stversions/a.jpg",
    ] {
        assert_eq!(excluded(path, &rules), Ok(true), "{path}");
    }
    for path in [
        "/photos/eaDir/a.jpg",
        "/photos/a_._b.jpg",
        "/photos/a.jpg",
        "/photos/._dir/a.jpg",
    ] {
        assert_eq!(excluded(path, &rules), Ok(false), "{path}");
    }
    assert_eq!(
        excluded("/photos/a.jpg", &["**/*.jpg".into()]),
        Err(DbMoveError::ExclusionRules)
    );
    assert!(!covered("/photos.old/a.jpg", &["/photos".into()]));
}
