//! 数据库连接配置的导入 / 导出边界。
//!
//! V2 使用 PBKDF2 派生密钥并以 AES-256-GCM 加密；导入继续兼容早期 V1 明文文件。

use std::collections::HashSet;
use std::fmt;

use aes_gcm::aead::rand_core::RngCore as _;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use ramag_domain::entities::{ConnectionConfig, MAX_CONNECTION_CONFIGS};
use serde::de::{IgnoredAny, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

/// V2 的 hex 密文约为内部 JSON 的两倍；该上限覆盖存储层容量并留有余量。
pub const MAX_IMPORT_FILE_BYTES: u64 = 144 * 1024 * 1024;
/// 限制异常口令输入放大 KDF 成本。
pub const MAX_TRANSFER_PASSPHRASE_BYTES: usize = 1024;
const MIN_EXPORT_PASSPHRASE_CHARS: usize = 8;
const PLAIN_EXPORT_VERSION: u32 = 1;
const ENCRYPTED_EXPORT_VERSION: u32 = 2;
const KDF_ITERATIONS: u32 = 600_000;
const KDF_ITERATIONS_MIN: u32 = 1_000;
const KDF_ITERATIONS_MAX: u32 = 5_000_000;

#[derive(Debug)]
pub enum PreparedConnectionImport {
    Plain {
        valid: Vec<ConnectionConfig>,
        skipped: Vec<String>,
    },
    Encrypted(String),
}

#[derive(Deserialize)]
struct VersionProbe {
    version: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct ConnectionsFile {
    version: u32,
    #[serde(deserialize_with = "deserialize_connections")]
    connections: Vec<ConnectionConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
struct EncryptedConnectionsFile {
    version: u32,
    kdf_salt: String,
    kdf_iterations: u32,
    nonce: String,
    ciphertext: String,
}

fn deserialize_connections<'de, D>(deserializer: D) -> Result<Vec<ConnectionConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ConnectionsVisitor;

    impl<'de> Visitor<'de> for ConnectionsVisitor {
        type Value = Vec<ConnectionConfig>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "不超过 {MAX_CONNECTION_CONFIGS} 条连接配置")
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let capacity = sequence
                .size_hint()
                .unwrap_or(0)
                .min(MAX_CONNECTION_CONFIGS);
            let mut connections = Vec::with_capacity(capacity);
            while connections.len() < MAX_CONNECTION_CONFIGS {
                let Some(connection) = sequence.next_element()? else {
                    return Ok(connections);
                };
                connections.push(connection);
            }
            if sequence.next_element::<IgnoredAny>()?.is_some() {
                return Err(serde::de::Error::custom(format!(
                    "连接数量超过 {MAX_CONNECTION_CONFIGS} 条上限"
                )));
            }
            Ok(connections)
        }
    }

    deserializer.deserialize_seq(ConnectionsVisitor)
}

fn serialize_connections(connections: &[ConnectionConfig]) -> Result<String, String> {
    serde_json::to_string_pretty(&ConnectionsFile {
        version: PLAIN_EXPORT_VERSION,
        connections: connections.to_vec(),
    })
    .map_err(|error| format!("序列化连接配置失败：{error}"))
}

fn parse_connections(raw: &str) -> Result<(Vec<ConnectionConfig>, Vec<String>), String> {
    let file: ConnectionsFile = serde_json::from_str(raw)
        .map_err(|error| format!("解析导入文件失败（须为 Ramag 导出的 JSON）：{error}"))?;
    if file.version != PLAIN_EXPORT_VERSION {
        return Err(format!(
            "导入文件版本不支持：{}（当前支持 {PLAIN_EXPORT_VERSION}）",
            file.version
        ));
    }

    let mut valid = Vec::new();
    let mut skipped = Vec::new();
    let mut seen_ids = HashSet::with_capacity(file.connections.len());
    for config in file.connections {
        match config.validate() {
            Ok(()) if seen_ids.insert(config.id.clone()) => valid.push(config),
            Ok(()) => skipped.push(format!("{}：连接 ID 重复", config.name)),
            Err(error) => skipped.push(format!("{}：{error}", config.name)),
        }
    }
    Ok((valid, skipped))
}

/// 识别导入格式。明文文件立即校验，加密文件只验证外层结构。
pub fn prepare_connection_import(raw: String) -> Result<PreparedConnectionImport, String> {
    let probe: VersionProbe = serde_json::from_str(&raw)
        .map_err(|error| format!("解析导入文件失败（须为 Ramag 导出的 JSON）：{error}"))?;
    match probe.version {
        PLAIN_EXPORT_VERSION => {
            let (valid, skipped) = parse_connections(&raw)?;
            Ok(PreparedConnectionImport::Plain { valid, skipped })
        }
        ENCRYPTED_EXPORT_VERSION => {
            serde_json::from_str::<EncryptedConnectionsFile>(&raw)
                .map_err(|error| format!("解析加密导出文件失败：{error}"))?;
            Ok(PreparedConnectionImport::Encrypted(raw))
        }
        version => Err(format!(
            "导入文件版本不支持：{version}（当前支持 V{PLAIN_EXPORT_VERSION} / V{ENCRYPTED_EXPORT_VERSION}）"
        )),
    }
}

/// 校验导出口令；导出文件包含密码，因此拒绝过短口令。
pub fn validate_connection_export_passphrase(passphrase: &str) -> Result<(), String> {
    if passphrase.len() > MAX_TRANSFER_PASSPHRASE_BYTES {
        return Err(format!(
            "导出口令超过 {MAX_TRANSFER_PASSPHRASE_BYTES} bytes 上限"
        ));
    }
    if passphrase.chars().count() < MIN_EXPORT_PASSPHRASE_CHARS {
        return Err(format!(
            "导出口令至少需要 {MIN_EXPORT_PASSPHRASE_CHARS} 个字符"
        ));
    }
    Ok(())
}

/// 加密导出全部连接。文件内不保留任何明文连接字段。
pub fn encrypt_connection_export(
    connections: &[ConnectionConfig],
    passphrase: &str,
) -> Result<String, String> {
    validate_connection_export_passphrase(passphrase)?;
    encrypt_connection_export_with(connections, passphrase, KDF_ITERATIONS)
}

fn encrypt_connection_export_with(
    connections: &[ConnectionConfig],
    passphrase: &str,
    iterations: u32,
) -> Result<String, String> {
    let plain = serialize_connections(connections)?;
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let key = derive_key(passphrase, &salt, iterations);
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|error| format!("构造加密密钥失败：{error}"))?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plain.as_bytes())
        .map_err(|error| format!("加密导出内容失败：{error}"))?;
    let content = serde_json::to_string_pretty(&EncryptedConnectionsFile {
        version: ENCRYPTED_EXPORT_VERSION,
        kdf_salt: hex::encode(salt),
        kdf_iterations: iterations,
        nonce: hex::encode(nonce),
        ciphertext: hex::encode(ciphertext),
    })
    .map_err(|error| format!("序列化导出文件失败：{error}"))?;
    if content.len() as u64 > MAX_IMPORT_FILE_BYTES {
        return Err(format!(
            "导出文件超过 {} MiB 上限，无法保证可重新导入",
            MAX_IMPORT_FILE_BYTES / 1024 / 1024
        ));
    }
    Ok(content)
}

/// 解密并校验连接配置。口令错误与文件损坏统一由 GCM 认证层拒绝。
pub fn decrypt_connection_import(
    raw: &str,
    passphrase: &str,
) -> Result<(Vec<ConnectionConfig>, Vec<String>), String> {
    let file: EncryptedConnectionsFile =
        serde_json::from_str(raw).map_err(|error| format!("解析加密导出文件失败：{error}"))?;
    if file.version != ENCRYPTED_EXPORT_VERSION {
        return Err(format!("加密文件版本不支持：{}", file.version));
    }
    if !(KDF_ITERATIONS_MIN..=KDF_ITERATIONS_MAX).contains(&file.kdf_iterations) {
        return Err(format!(
            "加密文件 KDF 迭代次数异常：{}",
            file.kdf_iterations
        ));
    }
    let salt = hex::decode(&file.kdf_salt).map_err(|_| "加密文件 salt 字段无效".to_string())?;
    let nonce = hex::decode(&file.nonce).map_err(|_| "加密文件 nonce 字段无效".to_string())?;
    if salt.len() != 16 {
        return Err("加密文件 salt 长度无效".into());
    }
    if nonce.len() != 12 {
        return Err("加密文件 nonce 长度无效".into());
    }
    let ciphertext =
        hex::decode(&file.ciphertext).map_err(|_| "加密文件密文字段无效".to_string())?;
    let key = derive_key(passphrase, &salt, file.kdf_iterations);
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|error| format!("构造解密密钥失败：{error}"))?;
    let plain = cipher
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_slice())
        .map_err(|_| "口令错误或文件已损坏".to_string())?;
    let plain = String::from_utf8(plain).map_err(|_| "解密结果不是有效文本".to_string())?;
    parse_connections(&plain)
}

fn derive_key(passphrase: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(passphrase.as_bytes(), salt, iterations, &mut key);
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> ConnectionConfig {
        let mut config = ConnectionConfig::new_mongodb(name, "127.0.0.1", 27017);
        config.username = "root".into();
        config.password = "secret".into();
        config.database = Some("ramag_test".into());
        config.auth_source = Some("admin".into());
        config.remark = Some("roundtrip".into());
        config.environment = Some("dev".into());
        config.production = true;
        config.tls = true;
        config.tls_verify = ramag_domain::entities::TlsVerify::Ca;
        config.ca_cert_path = Some("/tmp/test-ca.pem".into());
        config.ssh_target = Some("jump-host".into());
        config.ssh_port = Some(2222);
        config
    }

    #[test]
    fn encrypted_roundtrip_keeps_connection_fields() {
        let expected = sample("main");
        let out =
            encrypt_connection_export_with(std::slice::from_ref(&expected), "password", 1_000)
                .expect("测试加密应成功");
        assert!(matches!(
            prepare_connection_import(out.clone()).expect("格式应可识别"),
            PreparedConnectionImport::Encrypted(_)
        ));
        let (valid, skipped) =
            decrypt_connection_import(&out, "password").expect("正确口令应可解密");
        assert_eq!(valid, vec![expected]);
        assert!(skipped.is_empty());
        assert!(decrypt_connection_import(&out, "wrong").is_err());
    }

    #[test]
    fn plain_import_skips_duplicate_and_invalid_connections() {
        let first = sample("first");
        let mut duplicate = first.clone();
        duplicate.name = "duplicate".into();
        let mut invalid = sample("invalid");
        invalid.port = 0;
        let raw = serde_json::to_string(&ConnectionsFile {
            version: PLAIN_EXPORT_VERSION,
            connections: vec![first, duplicate, invalid],
        })
        .expect("测试数据应可序列化");

        let PreparedConnectionImport::Plain { valid, skipped } =
            prepare_connection_import(raw).expect("V1 应继续兼容")
        else {
            panic!("V1 文件不应识别成加密格式");
        };
        assert_eq!(valid.len(), 1);
        assert_eq!(skipped.len(), 2);
    }

    #[test]
    fn hostile_limits_and_short_passphrase_are_rejected() {
        assert!(validate_connection_export_passphrase("short").is_err());
        assert!(validate_connection_export_passphrase("long-enough").is_ok());

        let out = encrypt_connection_export_with(&[sample("main")], "password", 1_000)
            .expect("测试加密应成功");
        let mut file: EncryptedConnectionsFile =
            serde_json::from_str(&out).expect("测试文件应可解析");
        file.kdf_iterations = u32::MAX;
        let raw = serde_json::to_string(&file).expect("测试文件应可序列化");
        assert!(decrypt_connection_import(&raw, "password").is_err());
    }

    #[test]
    fn unsupported_version_and_invalid_envelope_are_rejected() {
        assert!(prepare_connection_import(r#"{"version":99}"#.into()).is_err());

        let out = encrypt_connection_export_with(&[sample("main")], "password", 1_000)
            .expect("测试加密应成功");
        let mut file: EncryptedConnectionsFile =
            serde_json::from_str(&out).expect("测试文件应可解析");
        file.kdf_salt = "00".into();
        let raw = serde_json::to_string(&file).expect("测试文件应可序列化");
        assert!(decrypt_connection_import(&raw, "password").is_err());
    }

    #[test]
    fn excessive_connection_count_is_rejected_before_allocation_grows_unbounded() {
        let connections = (0..=MAX_CONNECTION_CONFIGS)
            .map(|index| sample(&format!("connection-{index}")))
            .collect();
        let raw = serde_json::to_string(&ConnectionsFile {
            version: PLAIN_EXPORT_VERSION,
            connections,
        })
        .expect("测试文件应可序列化");

        assert!(prepare_connection_import(raw).is_err());
    }
}
