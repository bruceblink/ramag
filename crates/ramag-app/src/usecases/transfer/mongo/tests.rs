use super::*;

#[test]
fn keyset_filter_wraps_literal() {
    assert!(keyset_filter(&None).is_null());
    let filter = keyset_filter(&Some(json!({"$oid": "0123456789abcdef01234567"})));
    assert_eq!(
        filter
            .pointer("/$expr/$gt/1/$literal/$oid")
            .and_then(Value::as_str),
        Some("0123456789abcdef01234567")
    );
    let tricky = keyset_filter(&Some(json!("$field")));
    assert_eq!(
        tricky
            .pointer("/$expr/$gt/1/$literal")
            .and_then(Value::as_str),
        Some("$field")
    );
}

#[test]
fn index_specs_drop_id_index_and_ns() {
    let response = json!({"cursor": {"firstBatch": [
        {"name": "_id_", "key": {"_id": 1}},
        {"name": "email_1", "key": {"email": 1}, "unique": true, "ns": "db.users"},
    ]}});
    let specs = filter_index_specs(&response);
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0]["name"], "email_1");
    assert!(specs[0].get("ns").is_none());
    assert_eq!(filter_index_specs(&json!({})).len(), 0);
}

#[test]
fn create_command_keeps_command_name_first() {
    let options = json!({"capped": true, "size": 1024})
        .as_object()
        .cloned()
        .unwrap();
    let command = create_collection_command("events", options);
    let fields = command.as_object().unwrap().keys().collect::<Vec<_>>();
    assert_eq!(fields.first().map(|field| field.as_str()), Some("create"));
    assert_eq!(command["create"], "events");
    assert_eq!(command["capped"], true);
}

#[test]
fn collection_scope_requires_valid_object_and_rejects_other_scopes() {
    let header = json!({"scope": "collection", "object": "events"});
    assert_eq!(scoped_collection(&header).unwrap(), Some("events"));
    assert!(scoped_collection(&json!({"scope": "collection"})).is_err());
    assert!(scoped_collection(&json!({"scope": "database"})).is_err());
    assert_eq!(scoped_collection(&json!({})).unwrap(), None);
}
