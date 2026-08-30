//! Kafka 集群配置 CRUD。整条配置经主密钥加密，避免认证参数和连接路径明文落盘。

use std::sync::Arc;

use parking_lot::RwLock;
use redb::{Database, ReadableDatabase as _, ReadableTable, TableDefinition};
use tracing::{debug, info};

use ramag_domain::entities::{KafkaClusterConfig, MAX_KAFKA_CLUSTERS};
use ramag_domain::error::{DomainError, Result};

use crate::encryption::Cipher;
use crate::repos::bounded_json;

const MAX_KAFKA_CLUSTER_RECORD_BYTES: usize = 1024 * 1024;
const MAX_KAFKA_CLUSTER_LIST_BYTES: usize = 64 * 1024 * 1024;

/// 键为 KafkaClusterId UUID，值为加密后的 KafkaClusterConfig JSON hex。
pub(crate) const KAFKA_CLUSTERS_TABLE: TableDefinition<&str, &str> =
    TableDefinition::new("kafka_clusters");

fn encode_cluster(config: &KafkaClusterConfig, cipher: &Cipher) -> Result<String> {
    config.validate().map_err(DomainError::InvalidConfig)?;
    let json = bounded_json::serialize(config, MAX_KAFKA_CLUSTER_RECORD_BYTES, "Kafka 集群配置")?;
    cipher.encrypt(&json)
}

fn decode_cluster(key: &str, value: &str, cipher: &Cipher) -> Result<KafkaClusterConfig> {
    bounded_json::ensure_len(
        value.len(),
        MAX_KAFKA_CLUSTER_RECORD_BYTES * 2 + 64,
        &format!("Kafka 集群配置 {key}"),
    )?;
    let json = cipher.decrypt(value).map_err(|error| {
        DomainError::Storage(format!("解密 Kafka 集群配置 {key} 失败：{error}"))
    })?;
    let config: KafkaClusterConfig = serde_json::from_str(&json).map_err(|error| {
        DomainError::Storage(format!("反序列化 Kafka 集群配置 {key} 失败：{error}"))
    })?;
    config.validate().map_err(|error| {
        DomainError::Storage(format!("解密后的 Kafka 集群配置 {key} 无效：{error}"))
    })?;
    if config.id.to_string() != key {
        return Err(DomainError::Storage(format!(
            "Kafka 集群配置键与内容 ID 不一致：{key}"
        )));
    }
    Ok(config)
}

/// 读取并按名称排序所有 Kafka 集群配置，集合大小受数量和字节预算共同限制。
pub(crate) fn list(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
) -> Result<Vec<KafkaClusterConfig>> {
    let read_txn = db
        .begin_read()
        .map_err(|error| DomainError::Storage(format!("启动读事务失败：{error}")))?;
    let table = read_txn
        .open_table(KAFKA_CLUSTERS_TABLE)
        .map_err(|error| DomainError::Storage(format!("打开 kafka_clusters 表失败：{error}")))?;
    let cipher = cipher.read();
    let mut clusters = Vec::new();
    let mut retained_bytes = 0usize;
    for entry in table
        .iter()
        .map_err(|error| DomainError::Storage(format!("遍历 Kafka 集群配置失败：{error}")))?
    {
        let (key, value) = entry
            .map_err(|error| DomainError::Storage(format!("读取 Kafka 集群配置失败：{error}")))?;
        let (_, next_bytes) = bounded_json::next_collection_budget(
            clusters.len(),
            retained_bytes,
            value.value().len(),
            MAX_KAFKA_CLUSTERS,
            MAX_KAFKA_CLUSTER_LIST_BYTES,
            "Kafka 集群配置列表",
        )?;
        retained_bytes = next_bytes;
        clusters.push(decode_cluster(key.value(), value.value(), &cipher)?);
    }
    clusters.sort_by(|left, right| left.name.cmp(&right.name));
    debug!(
        operation = "kafka_cluster_list",
        count = clusters.len(),
        "kafka cluster listing completed"
    );
    Ok(clusters)
}

pub(crate) fn get(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    id: String,
) -> Result<Option<KafkaClusterConfig>> {
    let read_txn = db
        .begin_read()
        .map_err(|error| DomainError::Storage(format!("启动读事务失败：{error}")))?;
    let table = read_txn
        .open_table(KAFKA_CLUSTERS_TABLE)
        .map_err(|error| DomainError::Storage(format!("打开 kafka_clusters 表失败：{error}")))?;
    let value = table
        .get(id.as_str())
        .map_err(|error| DomainError::Storage(format!("读取 Kafka 集群配置 {id} 失败：{error}")))?;
    value
        .map(|value| decode_cluster(&id, value.value(), &cipher.read()))
        .transpose()
}

pub(crate) fn save(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    config: KafkaClusterConfig,
) -> Result<()> {
    let value = encode_cluster(&config, &cipher.read())?;
    let id = config.id.to_string();
    let write_txn = db
        .begin_write()
        .map_err(|error| DomainError::Storage(format!("启动写事务失败：{error}")))?;
    {
        let mut table = write_txn
            .open_table(KAFKA_CLUSTERS_TABLE)
            .map_err(|error| {
                DomainError::Storage(format!("打开 kafka_clusters 表失败：{error}"))
            })?;
        let mut count = 0usize;
        let mut total_bytes = 0usize;
        let mut replaced_bytes = None;
        for entry in table
            .iter()
            .map_err(|error| DomainError::Storage(format!("遍历 Kafka 集群配置失败：{error}")))?
        {
            let (key, existing) = entry.map_err(|error| {
                DomainError::Storage(format!("读取 Kafka 集群配置失败：{error}"))
            })?;
            (count, total_bytes) = bounded_json::next_collection_budget(
                count,
                total_bytes,
                existing.value().len(),
                MAX_KAFKA_CLUSTERS,
                MAX_KAFKA_CLUSTER_LIST_BYTES,
                "Kafka 集群配置列表",
            )?;
            if key.value() == id {
                replaced_bytes = Some(existing.value().len());
            }
        }
        let final_count = count + usize::from(replaced_bytes.is_none());
        let final_bytes = total_bytes
            .checked_sub(replaced_bytes.unwrap_or(0))
            .and_then(|bytes| bytes.checked_add(value.len()))
            .ok_or_else(|| DomainError::Storage("Kafka 集群配置列表总大小溢出".into()))?;
        bounded_json::ensure_collection_budget(
            final_count,
            final_bytes,
            MAX_KAFKA_CLUSTERS,
            MAX_KAFKA_CLUSTER_LIST_BYTES,
            "Kafka 集群配置列表",
        )?;
        table.insert(id.as_str(), value.as_str()).map_err(|error| {
            DomainError::Storage(format!("写入 Kafka 集群配置 {id} 失败：{error}"))
        })?;
    }
    write_txn
        .commit()
        .map_err(|error| DomainError::Storage(format!("提交事务失败：{error}")))?;
    info!(operation = "kafka_cluster_save", cluster_id = %id, "kafka cluster saved");
    Ok(())
}

pub(crate) fn delete(db: Arc<Database>, id: String) -> Result<()> {
    let write_txn = db
        .begin_write()
        .map_err(|error| DomainError::Storage(format!("启动写事务失败：{error}")))?;
    {
        let mut table = write_txn
            .open_table(KAFKA_CLUSTERS_TABLE)
            .map_err(|error| {
                DomainError::Storage(format!("打开 kafka_clusters 表失败：{error}"))
            })?;
        table.remove(id.as_str()).map_err(|error| {
            DomainError::Storage(format!("删除 Kafka 集群配置 {id} 失败：{error}"))
        })?;
    }
    write_txn
        .commit()
        .map_err(|error| DomainError::Storage(format!("提交事务失败：{error}")))?;
    info!(operation = "kafka_cluster_delete", cluster_id = %id, "kafka cluster deleted");
    Ok(())
}

pub(crate) fn ensure_table(write_txn: &redb::WriteTransaction) -> Result<()> {
    let _ = write_txn
        .open_table(KAFKA_CLUSTERS_TABLE)
        .map_err(|error| DomainError::Storage(format!("打开 kafka_clusters 表失败：{error}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_record_does_not_contain_cluster_fields() {
        let cipher = Cipher::new(&[9; 32]);
        let mut config = KafkaClusterConfig::new("production", vec!["secret.example:9092".into()]);
        config.security_protocol = ramag_domain::entities::KafkaSecurityProtocol::SaslSsl;
        config.sasl_mechanism = Some(ramag_domain::entities::KafkaSaslMechanism::Plain);
        config.sasl_username = Some("app".into());
        config.sasl_password = Some("secret-password".into());
        let encoded = encode_cluster(&config, &cipher).unwrap();

        assert!(!encoded.contains("production"));
        assert!(!encoded.contains("secret.example"));
        assert!(!encoded.contains("secret-password"));
        assert_eq!(
            decode_cluster(&config.id.to_string(), &encoded, &cipher).unwrap(),
            config
        );
    }

    #[test]
    fn decode_rejects_key_content_id_mismatch() {
        let cipher = Cipher::new(&[9; 32]);
        let config = KafkaClusterConfig::new("local", vec!["localhost:9092".into()]);
        let encoded = encode_cluster(&config, &cipher).unwrap();

        assert!(decode_cluster("other-id", &encoded, &cipher).is_err());
    }
}
