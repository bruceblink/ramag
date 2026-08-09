use super::*;

#[test]
fn mysql_ddl_removes_fk_and_maps_only_reference() {
    let ddl = "CREATE TABLE `orders` (\n `id` bigint NOT NULL,\n `note` varchar(40) DEFAULT 'REFERENCES `orders`',\n PRIMARY KEY (`id`),\n CONSTRAINT `fk_self` FOREIGN KEY (`id`) REFERENCES `orders` (`id`) ON DELETE CASCADE\n) ENGINE=InnoDB";
    let mappings = HashMap::from([("orders".into(), "orders_copy".into())]);
    let mapped =
        rewrite_mysql_table_ddl(ddl, "old_db", "new_db", "orders", "orders_copy", &mappings)
            .unwrap();
    assert!(
        mapped
            .create_statement
            .starts_with("CREATE TABLE `new_db`.`orders_copy`")
    );
    assert!(mapped.create_statement.contains("'REFERENCES `orders`'"));
    assert!(!mapped.create_statement.contains("CONSTRAINT `fk_self`"));
    assert_eq!(
        mapped.foreign_key_statements,
        [
            "ALTER TABLE `new_db`.`orders_copy` ADD CONSTRAINT `fk_self` FOREIGN KEY (`id`) REFERENCES `new_db`.`orders_copy` (`id`) ON DELETE CASCADE;"
        ]
    );
}

#[test]
fn postgres_rewrite_preserves_literals_and_same_named_column() {
    let mappings = HashMap::from([("orders".into(), "orders_copy".into())]);
    let sql = "CREATE TABLE \"old\".\"orders\" (\"old\" text DEFAULT '\"old\".\"orders\"');";
    let mapped = rewrite_postgres_statement(sql, "old", "new", &mappings).unwrap();
    assert_eq!(
        mapped,
        "CREATE TABLE \"new\".\"orders_copy\" (\"old\" text DEFAULT '\"old\".\"orders\"');"
    );
}

#[test]
fn postgres_rewrite_maps_unqualified_fk_reference() {
    let mappings = HashMap::from([("parent".into(), "parent_copy".into())]);
    let sql = "ALTER TABLE \"old\".\"child\" ADD FOREIGN KEY (id) REFERENCES \"parent\"(id);";
    let mapped = rewrite_postgres_statement(sql, "old", "new", &mappings).unwrap();
    assert_eq!(
        mapped,
        "ALTER TABLE \"new\".\"child\" ADD FOREIGN KEY (id) REFERENCES \"new\".\"parent_copy\"(id);"
    );
}

#[test]
fn postgres_rewrite_maps_unquoted_catalog_identifiers() {
    let mappings = HashMap::from([
        ("child".into(), "child_copy".into()),
        ("parent".into(), "parent_copy".into()),
    ]);
    let foreign_key = "ALTER TABLE old.child ADD CONSTRAINT fk FOREIGN KEY (parent_id) REFERENCES old.parent(id);";
    let mapped = rewrite_postgres_statement(foreign_key, "old", "new", &mappings).unwrap();
    assert!(mapped.starts_with("ALTER TABLE \"new\".\"child_copy\""));
    assert!(mapped.contains("REFERENCES \"new\".\"parent_copy\"(id)"));

    let index = rewrite_postgres_statement(
        "CREATE INDEX idx_child ON old.child USING btree (id);",
        "old",
        "new",
        &mappings,
    )
    .unwrap();
    assert!(index.contains(" ON \"new\".\"child_copy\""));

    let enum_type = rewrite_postgres_statement(
        "CREATE TYPE old.status AS ENUM ('active');",
        "old",
        "new",
        &mappings,
    )
    .unwrap();
    assert!(enum_type.starts_with("CREATE TYPE \"new\".\"status\""));
}

#[test]
fn postgres_rewrite_maps_regclass_but_not_text_default() {
    let mappings = HashMap::from([("orders_id_seq".into(), "copy_id_seq".into())]);
    let sql = "CREATE TABLE \"old\".\"orders\" (id bigint DEFAULT nextval('\"old\".\"orders_id_seq\"'::regclass), note text DEFAULT 'old.orders_id_seq');";
    let mapped = rewrite_postgres_statement(sql, "old", "new", &mappings).unwrap();
    assert!(mapped.contains("nextval('\"new\".\"copy_id_seq\"'::regclass)"));
    assert!(mapped.contains("DEFAULT 'old.orders_id_seq'"));
}

#[test]
fn postgres_type_name_equal_to_table_is_not_renamed() {
    let mappings = HashMap::from([("orders".into(), "orders_copy".into())]);
    let sql = "CREATE TABLE \"old\".\"orders\" (state \"old\".\"orders\");";
    let mapped = rewrite_postgres_statement(sql, "old", "new", &mappings).unwrap();
    assert_eq!(
        mapped,
        "CREATE TABLE \"new\".\"orders_copy\" (state \"new\".\"orders\");"
    );
}

#[test]
fn malformed_ddl_is_rejected() {
    let error = rewrite_mysql_table_ddl(
        "CREATE TABLE x (`id` int",
        "a",
        "b",
        "x",
        "x",
        &HashMap::new(),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("括号"));
}
