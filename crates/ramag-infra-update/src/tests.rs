use super::metadata::{parse_manifest, parse_release_permalink};
use super::*;

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn parses_stable_manifest_and_digest() {
    let body = format!(
        r#"{{
            "schema_version":1,
            "version":"1.2.3",
            "tag_name":"v1.2.3",
            "notes":"changes",
            "published_at":"2026-08-06T00:00:00Z",
            "assets":[{{
                "name":"Ramag-1.2.3-macos-arm64.dmg",
                "size":123,
                "sha256":"{HASH}"
            }}]
        }}"#
    );
    let release = parse_manifest(body.as_bytes()).expect("manifest should parse");
    assert_eq!(release.version, "1.2.3");
    assert_eq!(release.notes, "changes");
    assert_eq!(release.assets[0].sha256.as_deref(), Some(HASH));
    assert_eq!(
        release.assets[0].download_url,
        "https://github.com/tools-rs/ramag/releases/download/v1.2.3/Ramag-1.2.3-macos-arm64.dmg"
    );
}

#[test]
fn rejects_prerelease_manifest() {
    let body = br#"{
        "schema_version":1,
        "version":"1.2.3-beta.1",
        "tag_name":"v1.2.3-beta.1",
        "assets":[]
    }"#;
    assert!(parse_manifest(body).is_err());
}

#[test]
fn latest_release_permalink_accepts_only_canonical_stable_tag() {
    let release = parse_release_permalink(
        &Url::parse("https://github.com/tools-rs/ramag/releases/tag/v1.2.3").expect("valid URL"),
    )
    .expect("stable release permalink");
    assert_eq!(release.version, "1.2.3");
    assert!(release.assets.is_empty());

    let prerelease = Url::parse("https://github.com/tools-rs/ramag/releases/tag/v1.2.3-beta.1")
        .expect("valid URL");
    assert!(parse_release_permalink(&prerelease).is_err());
    let foreign =
        Url::parse("https://example.com/tools-rs/ramag/releases/tag/v1.2.3").expect("valid URL");
    assert!(parse_release_permalink(&foreign).is_err());
}

#[test]
fn selects_exact_checksum_entry() {
    let body = format!(
        "{HASH}  first.dmg\nffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff  second.dmg\n"
    );
    assert_eq!(
        parse_checksum(body.as_bytes(), "first.dmg").expect("hash"),
        HASH
    );
    assert!(parse_checksum(body.as_bytes(), "missing.dmg").is_err());
}

#[test]
fn download_url_must_match_repository_tag_and_asset() {
    assert!(
        validate_download_url(
            "https://github.com/tools-rs/ramag/releases/download/v1.0.0/Ramag-1.0.0.dmg",
            "v1.0.0",
            "Ramag-1.0.0.dmg"
        )
        .is_ok()
    );
    assert!(
        validate_download_url(
            "https://example.com/Ramag-1.0.0.dmg",
            "v1.0.0",
            "Ramag-1.0.0.dmg"
        )
        .is_err()
    );
}

#[test]
fn cached_file_requires_exact_size_and_sha256() {
    use sha2::{Digest, Sha256};

    let directory = std::env::temp_dir().join(format!(
        "ramag-update-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).expect("create temp directory");
    let file = directory.join("update.bin");
    std::fs::write(&file, b"ramag-update").expect("write fixture");
    let expected = hex::encode(Sha256::digest(b"ramag-update"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create test runtime");

    assert!(
        runtime
            .block_on(verify_file(&file, 12, &expected))
            .expect("verify cached file")
    );
    assert!(
        !runtime
            .block_on(verify_file(&file, 11, &expected))
            .expect("reject wrong size")
    );
    assert!(
        !runtime
            .block_on(verify_file(&file, 12, HASH))
            .expect("reject wrong hash")
    );
    std::fs::remove_dir_all(directory).expect("remove temp directory");
}
