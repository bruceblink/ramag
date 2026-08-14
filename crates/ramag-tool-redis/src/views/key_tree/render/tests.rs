use super::*;
use ramag_domain::entities::KeyMeta;

#[test]
fn search_flatten_visits_each_matching_branch_once() {
    let keys = vec![
        KeyMeta::bare("user:1:profile"),
        KeyMeta::bare("user:2:settings"),
        KeyMeta::bare("session:abc"),
    ];
    let tree = super::super::tree::build_tree(&keys);
    let rows = flatten_visible_rows(
        &tree,
        &std::collections::HashSet::from(["17xxx27".to_string()]),
        &std::collections::HashSet::new(),
        "profile",
        false,
    );
    let paths: Vec<&str> = rows.iter().map(|row| row.full_path.as_str()).collect();

    assert_eq!(paths, vec!["user", "user:1", "user:1:profile"]);
    assert!(rows[0].is_expanded);
    assert!(rows[1].is_expanded);
    assert!(!rows[2].is_expanded);
}

#[test]
fn search_flatten_keeps_bare_key_without_type() {
    let tree = super::super::tree::build_tree(&[KeyMeta::bare("111")]);
    let rows = flatten_visible_rows(
        &tree,
        &std::collections::HashSet::new(),
        &std::collections::HashSet::new(),
        "111",
        false,
    );

    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_key);
    assert!(rows[0].leaf_type.is_none());
}

#[test]
fn search_result_can_collapse_and_expand_without_changing_normal_state() {
    let tree =
        super::super::tree::build_tree(&[KeyMeta::bare("zset:1001"), KeyMeta::bare("zset:1002")]);
    let normal_expanded = std::collections::HashSet::from(["other".to_string()]);
    let collapsed = std::collections::HashSet::from(["zset".to_string()]);

    let collapsed_rows = flatten_visible_rows(&tree, &normal_expanded, &collapsed, "1002", false);
    assert_eq!(collapsed_rows.len(), 1);
    assert_eq!(collapsed_rows[0].full_path.as_str(), "zset");
    assert!(!collapsed_rows[0].is_expanded);

    let expanded_rows = flatten_visible_rows(
        &tree,
        &normal_expanded,
        &std::collections::HashSet::new(),
        "1002",
        false,
    );
    let paths: Vec<&str> = expanded_rows
        .iter()
        .map(|row| row.full_path.as_str())
        .collect();
    assert_eq!(paths, vec!["zset", "zset:1002"]);
    assert!(expanded_rows[0].is_expanded);
    assert_eq!(
        normal_expanded,
        std::collections::HashSet::from(["other".to_string()])
    );
}

#[test]
fn visible_siblings_choose_branch_and_last_child_connectors() {
    let tree = super::super::tree::build_tree(&[KeyMeta::bare("root:a"), KeyMeta::bare("root:b")]);
    let rows = flatten_visible_rows(
        &tree,
        &std::collections::HashSet::from(["root".to_string()]),
        &std::collections::HashSet::new(),
        "",
        false,
    );

    assert_eq!(rows.len(), 3);
    assert!(rows[1].has_next_sibling);
    assert!(!rows[2].has_next_sibling);
}

#[test]
fn namespace_and_key_with_same_path_render_as_separate_rows() {
    let tree =
        super::super::tree::build_tree(&[KeyMeta::bare("17xxx27"), KeyMeta::bare("17xxx27:code")]);
    let rows = flatten_visible_rows(
        &tree,
        &std::collections::HashSet::from(["17xxx27".to_string()]),
        &std::collections::HashSet::new(),
        "",
        true,
    );

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].full_path.as_str(), "17xxx27");
    assert!(rows[0].is_namespace);
    assert!(!rows[0].is_key);
    assert_eq!(rows[1].full_path.as_str(), "17xxx27:code");
    assert!(rows[1].is_key);
    assert_eq!(rows[2].full_path.as_str(), "17xxx27");
    assert!(rows[2].is_key);
    assert!(!rows[2].is_namespace);
}

#[test]
fn default_mode_keeps_same_path_in_one_combined_row() {
    let tree =
        super::super::tree::build_tree(&[KeyMeta::bare("17xxx27"), KeyMeta::bare("17xxx27:code")]);
    let rows = flatten_visible_rows(
        &tree,
        &std::collections::HashSet::from(["17xxx27".to_string()]),
        &std::collections::HashSet::new(),
        "",
        false,
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].full_path.as_str(), "17xxx27");
    assert!(rows[0].is_namespace);
    assert!(rows[0].is_key);
    assert_eq!(rows[1].full_path.as_str(), "17xxx27:code");
}

#[test]
fn sunk_key_aligns_with_deepest_visible_leaf() {
    let tree = super::super::tree::build_tree(&[
        KeyMeta::bare("17xxx27"),
        KeyMeta::bare("17xxx27:_entry_:code"),
    ]);
    let rows = flatten_visible_rows(
        &tree,
        &std::collections::HashSet::from(["17xxx27".to_string(), "17xxx27:_entry_".to_string()]),
        &std::collections::HashSet::new(),
        "",
        true,
    );

    assert_eq!(rows.len(), 4);
    assert_eq!(rows[2].full_path.as_str(), "17xxx27:_entry_:code");
    assert_eq!(rows[3].full_path.as_str(), "17xxx27");
    assert_eq!(rows[3].depth, rows[2].depth);
}

#[test]
fn sunk_key_stays_hidden_until_namespace_is_expanded() {
    let tree = super::super::tree::build_tree(&[
        KeyMeta::bare("17xxx27"),
        KeyMeta::bare("17xxx27:_entry_:code"),
    ]);
    let rows = flatten_visible_rows(
        &tree,
        &std::collections::HashSet::from(["17xxx27".to_string()]),
        &std::collections::HashSet::new(),
        "",
        true,
    );

    assert_eq!(rows.len(), 2);
    assert!(rows[0].is_namespace);
    assert!(!rows[0].is_key);
    assert_eq!(rows[1].full_path.as_str(), "17xxx27:_entry_");
    assert!(rows[1].is_namespace);
    assert!(!rows[1].is_key);
}

#[test]
fn tree_type_badges_use_redis_abbreviations() {
    assert_eq!(tree_type_label(RedisType::String), "STR");
    assert_eq!(tree_type_label(RedisType::Hash), "HASH");
    assert_eq!(tree_type_label(RedisType::ZSet), "ZSET");
}
