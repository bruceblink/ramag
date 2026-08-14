use super::{
    VisibleRowsCacheEntry, VisibleRowsCacheKey, apply_resolved_key_type, build_tree,
    prune_expanded_for_tree, should_ensure_loaded,
};
use ramag_domain::entities::{KeyMeta, RedisType};
use std::collections::HashSet;
use std::rc::Rc;

#[test]
fn empty_but_successful_scan_is_not_reloaded_on_activation() {
    assert!(should_ensure_loaded(true, false, false));
    assert!(!should_ensure_loaded(true, true, false));
    assert!(!should_ensure_loaded(true, false, true));
    assert!(!should_ensure_loaded(false, false, false));
}

#[test]
fn visible_rows_cache_is_scoped_to_tree_expansion_and_query() {
    let rows = Rc::new(Vec::new());
    let key = VisibleRowsCacheKey {
        tree_revision: 4,
        expanded_revision: 2,
        query: "user".into(),
        sink_same_name_keys: false,
    };
    let cache = VisibleRowsCacheEntry {
        key: key.clone(),
        rows: rows.clone(),
        leaf_count: 3,
    };

    let cached = cache.get(&key);
    assert!(cached.is_some());
    if let Some((cached_rows, leaf_count)) = cached {
        assert!(Rc::ptr_eq(&cached_rows, &rows));
        assert_eq!(leaf_count, 3);
    }

    let mut changed = key;
    changed.expanded_revision += 1;
    assert!(cache.get(&changed).is_none());

    let mut changed_setting = changed;
    changed_setting.expanded_revision -= 1;
    changed_setting.sink_same_name_keys = true;
    assert!(cache.get(&changed_setting).is_none());
}

#[test]
fn rebuilding_tree_drops_expansion_paths_from_old_searches() {
    let tree = build_tree(&[KeyMeta::bare("user:1")]);
    let mut expanded = HashSet::from(["user".to_string(), "old:path".to_string()]);

    assert!(prune_expanded_for_tree(&tree, &mut expanded));
    assert_eq!(expanded, HashSet::from(["user".to_string()]));
}

#[test]
fn resolved_type_updates_only_the_matching_key() {
    let mut keys = vec![KeyMeta::bare("root"), KeyMeta::bare("root:code")];

    assert!(apply_resolved_key_type(&mut keys, "root", RedisType::ZSet));
    assert_eq!(keys[0].key_type, Some(RedisType::ZSet));
    assert_eq!(keys[1].key_type, None);
    assert!(!apply_resolved_key_type(&mut keys, "root", RedisType::ZSet));
    assert!(!apply_resolved_key_type(
        &mut keys,
        "missing",
        RedisType::Hash
    ));
}
