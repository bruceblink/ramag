use super::*;

#[test]
fn archive_links_cannot_escape_target_directory() {
    for target in ["/etc/passwd", "../secret", "a/../../secret", "a\\secret"] {
        assert!(validate_link_target(target).is_err(), "{target}");
    }
    assert!(validate_link_target("config/current.yml").is_ok());
}

#[test]
fn archive_path_budget_is_bounded() {
    let mut retained = 0;
    let path = async_std::path::Path::new("root/file");
    assert!(charge_path_bytes(&mut retained, "/root/file", path, 1024).is_ok());
    let limit = retained;
    assert!(charge_path_bytes(&mut retained, "/root/file", path, limit).is_err());
}
