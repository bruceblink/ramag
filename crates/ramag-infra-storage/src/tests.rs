//! 拆分后的测试模块。

use super::*;
use ramag_domain::entities::DriverKind;
use tempfile::TempDir;

/// 临时目录 + 固定密钥，不污染真实系统凭据库
fn make_test_storage() -> (RedbStorage, TempDir) {
    let tmp = TempDir::new().expect("创建临时目录失败");
    let path = tmp.path().join("test.redb");
    let key = [0x42u8; 32];
    let storage = RedbStorage::open_with_key(&path, &key).expect("打开测试 storage 失败");
    (storage, tmp)
}

#[cfg(unix)]
#[test]
fn database_file_is_private() {
    use std::os::unix::fs::PermissionsExt as _;

    let (storage, _tmp) = make_test_storage();
    let mode = std::fs::metadata(storage.path())
        .expect("读取测试数据库权限失败")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[cfg(unix)]
#[test]
fn database_path_rejects_symbolic_links() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("target.redb");
    let link = tmp.path().join("linked.redb");
    symlink(&target, &link).unwrap();

    let error = match RedbStorage::open_with_key(&link, &[0x42; 32]) {
        Ok(_) => panic!("符号链接数据库路径不应成功打开"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("数据库文件不能是符号链接"));
    assert!(!target.exists());
}

#[cfg(unix)]
#[test]
fn database_parent_rejects_symbolic_links() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let target_dir = tmp.path().join("target-dir");
    std::fs::create_dir(&target_dir).unwrap();
    let linked_dir = tmp.path().join("linked-dir");
    symlink(&target_dir, &linked_dir).unwrap();

    let error = match RedbStorage::open_with_key(&linked_dir.join("data.redb"), &[0x42; 32]) {
        Ok(_) => panic!("符号链接数据目录不应成功打开"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("数据目录不能是符号链接"));
    assert!(!target_dir.join("data.redb").exists());
}

#[tokio::test]
async fn only_encrypted_records_require_existing_master_key() {
    let (storage, _tmp) = make_test_storage();
    assert!(!database_has_encrypted_records(&storage.db).unwrap());
    storage.set_preference("theme", "dark").await.unwrap();
    assert!(!database_has_encrypted_records(&storage.db).unwrap());
    storage
        .save_connection(&sample_config("encrypted"))
        .await
        .unwrap();
    assert!(database_has_encrypted_records(&storage.db).unwrap());
    let path = storage.path().to_path_buf();
    drop(storage);
    assert!(RedbStorage::open_with_key(&path, &[0x24; 32]).is_err());
}

#[tokio::test]
async fn fresh_storage_returns_missing_preference() {
    let (storage, _tmp) = make_test_storage();

    assert!(matches!(
        storage.get_preference("never_saved").await,
        Ok(None)
    ));
}

#[test]
fn fresh_storage_initializes_complete_schema() {
    use std::collections::BTreeSet;

    use redb::TableHandle as _;

    let (storage, _tmp) = make_test_storage();
    let read_txn = storage.db.begin_read().unwrap();
    let actual = read_txn
        .list_tables()
        .unwrap()
        .map(|table| table.name().to_string())
        .collect::<BTreeSet<_>>();
    let expected = BTreeSet::from([
        "clip_by_hash".to_string(),
        "clip_by_time".to_string(),
        "clip_search_filters_v1".to_string(),
        "clip_search_meta".to_string(),
        "clip_uuid_meta".to_string(),
        "clips".to_string(),
        "connections".to_string(),
        "preferences".to_string(),
        "query_history".to_string(),
        "query_history_meta".to_string(),
        "repos".to_string(),
        "ssh_profiles".to_string(),
    ]);

    assert_eq!(actual, expected);
}

#[tokio::test]
async fn reopening_repairs_missing_schema_without_losing_existing_data() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("partial.redb");
    let key = [0x31; 32];
    let repo = RepoConfig::from_path(tmp.path().join("kept-repo").display().to_string());

    {
        let storage = RedbStorage::open_with_key(&path, &key).unwrap();
        storage.save_repo(&repo).await.unwrap();
    }
    {
        let db = Database::create(&path).unwrap();
        let write_txn = db.begin_write().unwrap();
        write_txn
            .delete_table(repos::prefs_repo::PREFERENCES_TABLE)
            .unwrap();
        write_txn.commit().unwrap();
    }

    let repaired = RedbStorage::open_with_key(&path, &key).unwrap();
    assert!(matches!(
        repaired.get_preference("never_saved").await,
        Ok(None)
    ));
    let repos = repaired.list_repos().await.unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0].id, repo.id);
}

#[tokio::test]
async fn preference_delete_removes_value() {
    let (storage, _tmp) = make_test_storage();
    assert!(storage.set_preference("draft", "text").await.is_ok());
    assert!(matches!(
        storage.get_preference("draft").await,
        Ok(Some(value)) if value == "text"
    ));
    assert!(storage.delete_preference("draft").await.is_ok());
    assert!(matches!(storage.get_preference("draft").await, Ok(None)));
}

fn sample_config(name: &str) -> ConnectionConfig {
    ConnectionConfig {
        id: ConnectionId::new(),
        name: name.to_string(),
        driver: DriverKind::Mysql,
        host: "127.0.0.1".into(),
        port: 3306,
        username: "root".into(),
        password: "secret-password".into(),
        database: Some("test".into()),
        auth_source: None,
        remark: None,
        environment: None,
        production: false,
        tls: false,
        tls_verify: Default::default(),
        ca_cert_path: None,
        ssh_target: None,
        ssh_port: None,
    }
}

#[tokio::test]
async fn save_and_list() {
    let (storage, _tmp) = make_test_storage();
    let cfg = sample_config("dev");

    storage.save_connection(&cfg).await.unwrap();
    let list = storage.list_connections().await.unwrap();

    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "dev");
    assert_eq!(list[0].password, "secret-password");
}

#[tokio::test]
async fn save_and_get_by_id() {
    let (storage, _tmp) = make_test_storage();
    let cfg = sample_config("prod");

    storage.save_connection(&cfg).await.unwrap();
    let got = storage.get_connection(&cfg.id).await.unwrap();
    assert!(got.is_some());
    assert_eq!(got.unwrap().name, "prod");
}

#[tokio::test]
async fn get_missing_returns_none() {
    let (storage, _tmp) = make_test_storage();
    let id = ConnectionId::new();
    let got = storage.get_connection(&id).await.unwrap();
    assert!(got.is_none());
}

#[tokio::test]
async fn delete_works() {
    let (storage, _tmp) = make_test_storage();
    let cfg = sample_config("a");
    storage.save_connection(&cfg).await.unwrap();
    assert_eq!(storage.list_connections().await.unwrap().len(), 1);

    storage.delete_connection(&cfg.id).await.unwrap();
    assert_eq!(storage.list_connections().await.unwrap().len(), 0);
}

#[tokio::test]
async fn list_sorted_by_name() {
    let (storage, _tmp) = make_test_storage();
    for n in &["zebra", "apple", "mongo"] {
        storage.save_connection(&sample_config(n)).await.unwrap();
    }
    let list = storage.list_connections().await.unwrap();
    let names: Vec<_> = list.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["apple", "mongo", "zebra"]);
}

#[tokio::test]
async fn update_existing() {
    let (storage, _tmp) = make_test_storage();
    let mut cfg = sample_config("dev");
    storage.save_connection(&cfg).await.unwrap();

    cfg.host = "10.0.0.1".to_string();
    storage.save_connection(&cfg).await.unwrap();

    let got = storage.get_connection(&cfg.id).await.unwrap().unwrap();
    assert_eq!(got.host, "10.0.0.1");
}

#[tokio::test]
async fn ssh_profiles_are_encrypted_and_round_trip() {
    let (storage, _tmp) = make_test_storage();
    let mut profile = SshProfile::new("production", "server.example.com");
    profile.username = "deploy".into();

    storage.save_ssh_profile(&profile).await.unwrap();
    let listed = storage.list_ssh_profiles().await.unwrap();
    assert_eq!(listed, vec![profile.clone()]);
    assert_eq!(
        storage.get_ssh_profile(&profile.id).await.unwrap(),
        Some(profile.clone())
    );

    let read_txn = storage.db.begin_read().unwrap();
    let table = read_txn
        .open_table(repos::ssh_profile_repo::SSH_PROFILES_TABLE)
        .unwrap();
    let raw = table.get(profile.id.to_string().as_str()).unwrap().unwrap();
    assert!(!raw.value().contains("server.example.com"));
    drop(raw);
    drop(table);
    drop(read_txn);

    storage.delete_ssh_profile(&profile.id).await.unwrap();
    assert!(storage.list_ssh_profiles().await.unwrap().is_empty());
}

#[tokio::test]
async fn batch_save_inserts_and_updates_atomically() {
    let (storage, _tmp) = make_test_storage();
    let mut existing = sample_config("existing");
    storage.save_connection(&existing).await.unwrap();

    existing.host = "10.0.0.8".into();
    let added = sample_config("added");
    storage
        .save_connections(&[existing.clone(), added.clone()])
        .await
        .unwrap();

    assert_eq!(
        storage
            .get_connection(&existing.id)
            .await
            .unwrap()
            .unwrap()
            .host,
        "10.0.0.8"
    );
    assert!(storage.get_connection(&added.id).await.unwrap().is_some());
    assert_eq!(storage.list_connections().await.unwrap().len(), 2);
}

#[tokio::test]
async fn invalid_batch_does_not_leave_partial_connection_updates() {
    let (storage, _tmp) = make_test_storage();
    let original = sample_config("original");
    storage.save_connection(&original).await.unwrap();

    let mut update = original.clone();
    update.host = "10.0.0.9".into();
    let mut invalid = sample_config("invalid");
    invalid.port = 0;

    assert!(
        storage
            .save_connections(&[update, invalid.clone()])
            .await
            .is_err()
    );
    assert_eq!(
        storage
            .get_connection(&original.id)
            .await
            .unwrap()
            .unwrap()
            .host,
        "127.0.0.1"
    );
    assert!(storage.get_connection(&invalid.id).await.unwrap().is_none());
    assert_eq!(storage.list_connections().await.unwrap().len(), 1);
}

fn sample_history(connection_id: ConnectionId, sql: &str) -> QueryRecord {
    QueryRecord::new_success(connection_id, "test", sql, 5, 1)
}

fn history_stats(storage: &RedbStorage) -> (u64, u64) {
    let txn = storage.db.begin_read().unwrap();
    let table = txn
        .open_table(repos::history_repo::HISTORY_META_TABLE)
        .unwrap();
    let count = table
        .get("record_count")
        .unwrap()
        .map_or(0, |value| value.value());
    let bytes = table
        .get("value_bytes")
        .unwrap()
        .map_or(0, |value| value.value());
    (count, bytes)
}

#[tokio::test]
async fn history_crud_and_connection_filter() {
    let (storage, _tmp) = make_test_storage();
    let first_connection = ConnectionId::new();
    let second_connection = ConnectionId::new();
    let mut first = sample_history(first_connection.clone(), "SELECT 1");
    first.executed_at = Utc::now() - Duration::seconds(1);
    let mut second = sample_history(second_connection.clone(), "SELECT 2");
    second.executed_at = Utc::now();

    storage.append_history(&first).await.unwrap();
    storage.append_history(&second).await.unwrap();
    let (count, bytes) = history_stats(&storage);
    assert_eq!(count, 2);
    assert!(bytes > 0);
    assert_eq!(storage.list_history(None, 10).await.unwrap().len(), 2);
    let newest = storage.list_history(None, 1).await.unwrap();
    assert_eq!(newest[0].id, second.id);
    assert!(storage.list_history(None, 0).await.unwrap().is_empty());

    let filtered = storage
        .list_history(Some(&first_connection), 10)
        .await
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, first.id);

    let bounded = storage.list_history_bounded(None, 10, 8).await.unwrap();
    assert_eq!(bounded.records.len(), 1);
    assert!(bounded.truncated);

    storage.delete_history(&first.id).await.unwrap();
    assert_eq!(history_stats(&storage).0, 1);
    assert_eq!(storage.list_history(None, 10).await.unwrap().len(), 1);
    storage
        .clear_history(Some(&second_connection))
        .await
        .unwrap();
    assert_eq!(history_stats(&storage), (0, 0));
    assert!(storage.list_history(None, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn corrupted_history_is_reported_instead_of_silently_skipped() {
    let (storage, _tmp) = make_test_storage();
    {
        let txn = storage.db.begin_write().unwrap();
        {
            let mut table = txn.open_table(repos::history_repo::HISTORY_TABLE).unwrap();
            table.insert("corrupt-key", "{not-json").unwrap();
        }
        txn.commit().unwrap();
    }

    let list_error = storage.list_history(None, 10).await.unwrap_err();
    assert!(list_error.to_string().contains("corrupt-key"));
    assert!(
        storage
            .clear_history(Some(&ConnectionId::new()))
            .await
            .is_err()
    );

    // 全量清空无需解析内容，应能作为损坏数据的恢复路径。
    storage.clear_history(None).await.unwrap();
    assert!(storage.list_history(None, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn corrupted_repo_record_blocks_unsafe_deduplication() {
    let (storage, _tmp) = make_test_storage();
    {
        let txn = storage.db.begin_write().unwrap();
        {
            let mut table = txn.open_table(repos::repo_repo::REPOS_TABLE).unwrap();
            table.insert("corrupt-repo", "{not-json").unwrap();
        }
        txn.commit().unwrap();
    }

    let repo = RepoConfig::from_path("/tmp/example-repo");
    let error = storage.save_repo(&repo).await.unwrap_err();
    assert!(error.to_string().contains("corrupt-repo"));
    assert!(storage.list_repos().await.is_err());
}

use chrono::{Duration, Utc};
use ramag_domain::entities::{ClipId, ClipKind};

mod clip_tests;
