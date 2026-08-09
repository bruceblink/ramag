use super::*;

#[test]
fn collection_cache_estimate_counts_reserved_structs_and_strings() {
    let mut collections = Vec::with_capacity(8);
    collections.push(MongoCollection {
        name: "users".to_string(),
        database: "app".to_string(),
        is_view: false,
    });

    let bytes = collection_list_retained_bytes("app", &collections, collections.capacity());

    assert!(bytes >= 8 * std::mem::size_of::<MongoCollection>() + "users".len() + 2 * 3);
}

#[test]
fn replacement_budget_subtracts_previous_entry_before_checking_limit() {
    assert_eq!(prospective_collection_bytes(100, 40, 60), 120);
    assert_eq!(prospective_collection_bytes(10, 20, 5), 5);
    assert_eq!(prospective_collection_bytes(usize::MAX, 0, 1), usize::MAX);
}

#[test]
fn configured_database_is_inserted_once_without_resorting() {
    let mut databases = vec![
        MongoDatabase {
            name: "admin".into(),
            size_on_disk: None,
            empty: false,
        },
        MongoDatabase {
            name: "users".into(),
            size_on_disk: None,
            empty: false,
        },
    ];

    insert_configured_database(&mut databases, Some("app".into()));
    insert_configured_database(&mut databases, Some("users".into()));

    assert_eq!(
        databases
            .iter()
            .map(|database| database.name.as_str())
            .collect::<Vec<_>>(),
        ["admin", "app", "users"]
    );
    assert!(databases[1].empty);
}
