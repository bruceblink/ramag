use ramag_domain::entities::{
    KafkaClusterConfig, KafkaSaslMechanism, KafkaSecurityProtocol, MAX_KAFKA_CLUSTER_NAME_BYTES,
};
use ramag_domain::traits::Storage;
use redb::ReadableDatabase;

use crate::RedbStorage;
use crate::repos;

use super::make_test_storage;

fn sample_cluster(name: &str, bootstrap_server: &str) -> KafkaClusterConfig {
    KafkaClusterConfig::new(name, vec![bootstrap_server.to_string()])
}

#[tokio::test]
async fn kafka_clusters_are_encrypted_sorted_and_round_trip() {
    let (storage, _tmp) = make_test_storage();
    let mut secure = sample_cluster("production", "secret.example.com:9092");
    secure.security_protocol = KafkaSecurityProtocol::SaslSsl;
    secure.sasl_mechanism = Some(KafkaSaslMechanism::ScramSha512);
    secure.sasl_username = Some("application".into());
    secure.sasl_password = Some("secret-password".into());
    secure.tls.ca_cert_path = Some("C:\\certs\\ca.pem".into());
    let local = sample_cluster("local", "127.0.0.1:9092");

    storage.save_kafka_cluster(&secure).await.unwrap();
    storage.save_kafka_cluster(&local).await.unwrap();

    let listed = storage.list_kafka_clusters().await.unwrap();
    assert_eq!(listed, vec![local.clone(), secure.clone()]);
    assert_eq!(
        storage.get_kafka_cluster(&secure.id).await.unwrap(),
        Some(secure.clone())
    );

    let read_txn = storage.db.begin_read().unwrap();
    let table = read_txn
        .open_table(repos::kafka_cluster_repo::KAFKA_CLUSTERS_TABLE)
        .unwrap();
    let raw = table.get(secure.id.to_string().as_str()).unwrap().unwrap();
    assert!(!raw.value().contains("production"));
    assert!(!raw.value().contains("secret.example.com"));
    assert!(!raw.value().contains("secret-password"));
}

#[tokio::test]
async fn kafka_cluster_delete_and_missing_lookup_are_scoped_to_one_profile() {
    let (storage, _tmp) = make_test_storage();
    let deleted = sample_cluster("deleted", "deleted.example:9092");
    let preserved = sample_cluster("preserved", "preserved.example:9092");
    storage.save_kafka_cluster(&deleted).await.unwrap();
    storage.save_kafka_cluster(&preserved).await.unwrap();

    storage.delete_kafka_cluster(&deleted.id).await.unwrap();

    assert!(
        storage
            .get_kafka_cluster(&deleted.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        storage.list_kafka_clusters().await.unwrap(),
        vec![preserved.clone()]
    );
    assert!(
        storage
            .get_kafka_cluster(&KafkaClusterConfig::new("missing", vec!["missing:9092".into()]).id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn kafka_clusters_survive_reopen_and_require_the_original_key() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("kafka.redb");
    let key = [0x52; 32];
    let cluster = sample_cluster("reopen", "localhost:9092");

    {
        let storage = RedbStorage::open_with_key(&path, &key).unwrap();
        storage.save_kafka_cluster(&cluster).await.unwrap();
        assert!(crate::database_has_encrypted_records(&storage.db).unwrap());
    }

    let reopened = RedbStorage::open_with_key(&path, &key).unwrap();
    assert_eq!(
        reopened.get_kafka_cluster(&cluster.id).await.unwrap(),
        Some(cluster)
    );
    drop(reopened);
    assert!(RedbStorage::open_with_key(&path, &[0x53; 32]).is_err());
}

#[tokio::test]
async fn invalid_kafka_cluster_is_rejected_before_writing() {
    let (storage, _tmp) = make_test_storage();
    let mut invalid = sample_cluster("invalid", "localhost:9092");
    invalid.name = "n".repeat(MAX_KAFKA_CLUSTER_NAME_BYTES + 1);

    assert!(storage.save_kafka_cluster(&invalid).await.is_err());
    assert!(storage.list_kafka_clusters().await.unwrap().is_empty());
}
