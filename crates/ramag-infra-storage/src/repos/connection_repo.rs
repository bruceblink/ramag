//! 连接配置 CRUD。密码经 Cipher 加密为 hex 落盘；密钥变化 / 数据损坏读取时抛 Storage 错

use std::sync::Arc;

use parking_lot::RwLock;
use redb::{Database, ReadableDatabase as _, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use ramag_domain::entities::{ConnectionConfig, ConnectionId};
use ramag_domain::error::{DomainError, Result};

use crate::encryption::Cipher;
use crate::repos::bounded_json;

const MAX_CONNECTION_RECORD_BYTES: usize = 1024 * 1024;
const MAX_CONNECTION_RECORDS: usize = 2048;
const MAX_CONNECTION_LIST_BYTES: usize = 64 * 1024 * 1024;

/// key=ConnectionId UUID，value=`EncryptedConnection` JSON
pub(crate) const CONNECTIONS_TABLE: TableDefinition<&str, &str> =
    TableDefinition::new("connections");

/// 落盘版连接，密码加密为 hex
#[derive(Debug, Serialize, Deserialize)]
struct EncryptedConnection {
    id: ConnectionId,
    name: String,
    driver: ramag_domain::entities::DriverKind,
    host: String,
    port: u16,
    username: String,
    /// 加密密码（hex）；明文不入库
    password_enc: String,
    database: Option<String>,
    #[serde(default)]
    auth_source: Option<String>,
    remark: Option<String>,
    #[serde(default)]
    production: bool,
    /// TLS 开关、验证等级与自定义 CA 路径（明文元数据，非机密；老数据缺省 = 关 / Full）
    #[serde(default)]
    tls: bool,
    #[serde(default)]
    tls_verify: ramag_domain::entities::TlsVerify,
    #[serde(default)]
    ca_cert_path: Option<String>,
    /// SSH 跳板目标与端口（认证走系统 ssh，不存任何 SSH 凭证；老数据缺省 = 不走隧道）
    #[serde(default)]
    ssh_target: Option<String>,
    #[serde(default)]
    ssh_port: Option<u16>,
}

impl EncryptedConnection {
    fn from_plain(plain: &ConnectionConfig, cipher: &Cipher) -> Result<Self> {
        plain.validate().map_err(DomainError::InvalidConfig)?;
        Ok(Self {
            id: plain.id.clone(),
            name: plain.name.clone(),
            driver: plain.driver,
            host: plain.host.clone(),
            port: plain.port,
            username: plain.username.clone(),
            password_enc: cipher.encrypt(&plain.password)?,
            database: plain.database.clone(),
            auth_source: plain.auth_source.clone(),
            remark: plain.remark.clone(),
            production: plain.production,
            tls: plain.tls,
            tls_verify: plain.tls_verify,
            ca_cert_path: plain.ca_cert_path.clone(),
            ssh_target: plain.ssh_target.clone(),
            ssh_port: plain.ssh_port,
        })
    }

    fn into_plain(self, cipher: &Cipher) -> Result<ConnectionConfig> {
        let plain = ConnectionConfig {
            id: self.id,
            name: self.name,
            driver: self.driver,
            host: self.host,
            port: self.port,
            username: self.username,
            password: cipher.decrypt(&self.password_enc)?,
            database: self.database,
            auth_source: self.auth_source,
            remark: self.remark,
            production: self.production,
            tls: self.tls,
            tls_verify: self.tls_verify,
            ca_cert_path: self.ca_cert_path,
            ssh_target: self.ssh_target,
            ssh_port: self.ssh_port,
        };
        plain
            .validate()
            .map_err(|error| DomainError::Storage(format!("解密后的连接配置无效：{error}")))?;
        Ok(plain)
    }
}

fn decode_connection(key: &str, value: &str, cipher: &Cipher) -> Result<ConnectionConfig> {
    bounded_json::ensure_len(
        value.len(),
        MAX_CONNECTION_RECORD_BYTES,
        &format!("连接记录 {key}"),
    )?;
    let encrypted: EncryptedConnection = serde_json::from_str(value)
        .map_err(|error| DomainError::Storage(format!("反序列化连接 {key} 失败：{error}")))?;
    encrypted.into_plain(cipher)
}

pub(crate) fn list(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
) -> Result<Vec<ConnectionConfig>> {
    let read_txn = db
        .begin_read()
        .map_err(|e| DomainError::Storage(format!("启动读事务失败：{e}")))?;
    let table = read_txn
        .open_table(CONNECTIONS_TABLE)
        .map_err(|e| DomainError::Storage(format!("打开表失败：{e}")))?;

    let cipher = cipher.read();
    let mut out = Vec::new();
    let mut retained_bytes = 0usize;
    for entry in table
        .iter()
        .map_err(|e| DomainError::Storage(e.to_string()))?
    {
        let (key, value) = entry.map_err(|e| DomainError::Storage(e.to_string()))?;
        let (_, next_bytes) = bounded_json::next_collection_budget(
            out.len(),
            retained_bytes,
            value.value().len(),
            MAX_CONNECTION_RECORDS,
            MAX_CONNECTION_LIST_BYTES,
            "连接列表",
        )?;
        retained_bytes = next_bytes;
        out.push(decode_connection(key.value(), value.value(), &cipher)?);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    debug!(count = out.len(), "list_connections done");
    Ok(out)
}

pub(crate) fn get(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    id: String,
) -> Result<Option<ConnectionConfig>> {
    let read_txn = db
        .begin_read()
        .map_err(|e| DomainError::Storage(format!("启动读事务失败：{e}")))?;
    let table = read_txn
        .open_table(CONNECTIONS_TABLE)
        .map_err(|e| DomainError::Storage(format!("打开表失败：{e}")))?;

    match table
        .get(id.as_str())
        .map_err(|e| DomainError::Storage(e.to_string()))?
    {
        Some(v) => {
            let cipher = cipher.read();
            Ok(Some(decode_connection(&id, v.value(), &cipher)?))
        }
        None => Ok(None),
    }
}

pub(crate) fn save(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    config: ConnectionConfig,
) -> Result<()> {
    let cipher = cipher.read();
    let enc = EncryptedConnection::from_plain(&config, &cipher)?;
    let json = bounded_json::serialize(&enc, MAX_CONNECTION_RECORD_BYTES, "连接记录")?;
    let id_str = config.id.to_string();

    let write_txn = db
        .begin_write()
        .map_err(|e| DomainError::Storage(format!("启动写事务失败：{e}")))?;
    {
        let mut table = write_txn
            .open_table(CONNECTIONS_TABLE)
            .map_err(|e| DomainError::Storage(format!("打开表失败：{e}")))?;

        let mut item_count = 0usize;
        let mut total_bytes = 0usize;
        let mut replaced_bytes = None;
        for entry in table
            .iter()
            .map_err(|error| DomainError::Storage(format!("遍历连接记录失败：{error}")))?
        {
            let (key, value) = entry
                .map_err(|error| DomainError::Storage(format!("读取连接记录失败：{error}")))?;
            bounded_json::ensure_len(
                value.value().len(),
                MAX_CONNECTION_RECORD_BYTES,
                &format!("连接记录 {}", key.value()),
            )?;
            (item_count, total_bytes) = bounded_json::next_collection_budget(
                item_count,
                total_bytes,
                value.value().len(),
                MAX_CONNECTION_RECORDS,
                MAX_CONNECTION_LIST_BYTES,
                "连接列表",
            )?;
            if key.value() == id_str {
                replaced_bytes = Some(value.value().len());
            }
        }
        let final_count = item_count.saturating_add(usize::from(replaced_bytes.is_none()));
        let final_bytes = total_bytes
            .saturating_sub(replaced_bytes.unwrap_or(0))
            .checked_add(json.len())
            .ok_or_else(|| DomainError::Storage("连接列表总数据大小溢出".into()))?;
        bounded_json::ensure_collection_budget(
            final_count,
            final_bytes,
            MAX_CONNECTION_RECORDS,
            MAX_CONNECTION_LIST_BYTES,
            "连接列表",
        )?;
        table
            .insert(id_str.as_str(), json.as_str())
            .map_err(|e| DomainError::Storage(format!("写入失败：{e}")))?;
    }
    write_txn
        .commit()
        .map_err(|e| DomainError::Storage(format!("提交事务失败：{e}")))?;

    info!(connection_id = %config.id, name = %config.name, "connection saved");
    Ok(())
}

pub(crate) fn delete(db: Arc<Database>, id: String) -> Result<()> {
    let write_txn = db
        .begin_write()
        .map_err(|e| DomainError::Storage(format!("启动写事务失败：{e}")))?;
    {
        let mut table = write_txn
            .open_table(CONNECTIONS_TABLE)
            .map_err(|e| DomainError::Storage(format!("打开表失败：{e}")))?;
        table
            .remove(id.as_str())
            .map_err(|e| DomainError::Storage(format!("删除失败：{e}")))?;
    }
    write_txn
        .commit()
        .map_err(|e| DomainError::Storage(format!("提交事务失败：{e}")))?;

    info!(connection_id = %id, "connection deleted");
    Ok(())
}

/// 由 lib.rs 在 open 时调
pub(crate) fn ensure_table(write_txn: &redb::WriteTransaction) -> Result<()> {
    let _ = write_txn
        .open_table(CONNECTIONS_TABLE)
        .map_err(|e| DomainError::Storage(format!("打开 connections 表失败：{e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::MAX_CONNECTION_PASSWORD_BYTES;

    #[test]
    fn encryption_boundary_rejects_invalid_plain_and_decrypted_configs() {
        let cipher = Cipher::new(&[7; 32]);
        let mut config = ConnectionConfig::new_mysql("local", "127.0.0.1", 3306, "root");
        config.password = "x".repeat(MAX_CONNECTION_PASSWORD_BYTES + 1);
        assert!(EncryptedConnection::from_plain(&config, &cipher).is_err());

        let valid = ConnectionConfig::new_mysql("local", "127.0.0.1", 3306, "root");
        let encrypted = EncryptedConnection::from_plain(&valid, &cipher);
        assert!(encrypted.is_ok());
        if let Ok(mut encrypted) = encrypted {
            encrypted.port = 0;
            assert!(encrypted.into_plain(&cipher).is_err());
        }
    }
}
