//! 连接配置导入 / 导出的序列化边界。导出为口令加密 JSON（PBKDF2 派生密钥 + AES-256-GCM），
//! 导入兼容加密（V2）与早期明文（V1）两种格式；纯函数便于测试

use std::collections::HashSet;
use std::fmt;

use aes_gcm::aead::rand_core::RngCore as _;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use ramag_domain::entities::{ConnectionConfig, MAX_CONNECTION_CONFIGS};
use serde::de::{IgnoredAny, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

/// V2 使用 hex 保存密文，最坏约为内部 JSON 的两倍；该上限覆盖存储层 64 MiB 总量并留余量。
pub(super) const MAX_IMPORT_FILE_BYTES: u64 = 144 * 1024 * 1024;
/// 口令输入资源边界；正常口令远小于此，避免异常输入放大 KDF 成本。
pub(super) const MAX_TRANSFER_PASSPHRASE_BYTES: usize = 1024;
const MIN_EXPORT_PASSPHRASE_CHARS: usize = 8;
/// 明文格式版本（V1，仅导入兼容）；不认识的版本显式拒绝而非猜测解析
pub(super) const EXPORT_VERSION: u32 = 1;
/// 加密格式版本（V2，导出唯一格式）
const ENCRYPTED_EXPORT_VERSION: u32 = 2;
/// PBKDF2-HMAC-SHA256 迭代次数（OWASP 推荐量级）；解密按文件内记录值执行
const KDF_ITERATIONS: u32 = 600_000;
/// 解密时接受的迭代范围：防恶意文件用超大迭代造成长时间挂起
const KDF_ITERATIONS_MIN: u32 = 1_000;
const KDF_ITERATIONS_MAX: u32 = 5_000_000;

#[derive(Debug)]
pub(super) enum PreparedImport {
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
pub(super) struct ConnectionsFile {
    pub version: u32,
    #[serde(deserialize_with = "deserialize_connections")]
    pub connections: Vec<ConnectionConfig>,
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

/// 序列化导出内容（pretty JSON，便于人工检查与版本管理）
pub(super) fn serialize_connections(connections: &[ConnectionConfig]) -> Result<String, String> {
    serde_json::to_string_pretty(&ConnectionsFile {
        version: EXPORT_VERSION,
        connections: connections.to_vec(),
    })
    .map_err(|error| format!("序列化连接配置失败：{error}"))
}

/// 解析导入内容：版本必须匹配；逐条过域校验，非法条目跳过并返回原因。
/// 返回 (有效配置, 跳过原因列表)
pub(super) fn parse_connections(raw: &str) -> Result<(Vec<ConnectionConfig>, Vec<String>), String> {
    let file: ConnectionsFile = serde_json::from_str(raw)
        .map_err(|error| format!("解析导入文件失败（须为 Ramag 导出的 JSON）：{error}"))?;
    if file.version != EXPORT_VERSION {
        return Err(format!(
            "导入文件版本不支持：{}（当前支持 {EXPORT_VERSION}）",
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

/// 在阻塞线程完成格式识别与明文解析，UI 线程只处理轻量分支。
pub(super) fn prepare_import(raw: String) -> Result<PreparedImport, String> {
    let probe: VersionProbe = serde_json::from_str(&raw)
        .map_err(|error| format!("解析导入文件失败（须为 Ramag 导出的 JSON）：{error}"))?;
    match probe.version {
        EXPORT_VERSION => {
            let (valid, skipped) = parse_connections(&raw)?;
            Ok(PreparedImport::Plain { valid, skipped })
        }
        ENCRYPTED_EXPORT_VERSION => {
            serde_json::from_str::<EncryptedConnectionsFile>(&raw)
                .map_err(|error| format!("解析加密导出文件失败：{error}"))?;
            Ok(PreparedImport::Encrypted(raw))
        }
        version => Err(format!(
            "导入文件版本不支持：{version}（当前支持 V{EXPORT_VERSION} / V{ENCRYPTED_EXPORT_VERSION}）"
        )),
    }
}

/// 加密导出文件：内层为 V1 明文 JSON，整体经口令派生密钥加密
#[derive(Debug, Serialize, Deserialize)]
struct EncryptedConnectionsFile {
    version: u32,
    /// PBKDF2 盐（hex）
    kdf_salt: String,
    kdf_iterations: u32,
    /// AES-GCM nonce（hex，12 字节）
    nonce: String,
    /// 密文（hex）
    ciphertext: String,
}

/// 口令加密导出：文件不含任何明文字段，遗失口令无法解密
pub(super) fn encrypt_connections(
    connections: &[ConnectionConfig],
    passphrase: &str,
) -> Result<String, String> {
    validate_export_passphrase(passphrase)?;
    encrypt_connections_with(connections, passphrase, KDF_ITERATIONS)
}

pub(super) fn validate_export_passphrase(passphrase: &str) -> Result<(), String> {
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

/// 迭代次数可注入：测试用小值避免 debug 构建下的分钟级 KDF
fn encrypt_connections_with(
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

/// 解密并解析加密导出文件；口令错误与文件损坏在 GCM 认证层一并暴露
pub(super) fn decrypt_connections(
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
        let mut config = ConnectionConfig::new_mysql(name, "127.0.0.1", 3306, "root");
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
    fn export_import_roundtrip_keeps_all_fields() {
        let out = serialize_connections(&[sample("a"), sample("b")]).unwrap();
        let (valid, skipped) = parse_connections(&out).unwrap();
        assert_eq!(valid.len(), 2);
        assert!(skipped.is_empty());
        assert_eq!(valid[0].name, "a");
        assert_eq!(valid[0].password, "secret");
        assert_eq!(valid[0].environment.as_deref(), Some("dev"));
        assert_eq!(valid[0].auth_source.as_deref(), Some("admin"));
        assert!(valid[0].production);
        assert!(valid[0].tls);
        assert_eq!(valid[0].tls_verify, ramag_domain::entities::TlsVerify::Ca);
        assert_eq!(valid[0].ssh_port, Some(2222));
    }

    #[test]
    fn unsupported_version_rejected() {
        let raw = r#"{"version":99,"connections":[]}"#;
        assert!(parse_connections(raw).unwrap_err().contains("99"));
    }

    #[test]
    fn invalid_entries_skipped_with_reason() {
        let mut bad = sample("bad");
        bad.port = 0;
        let out = serialize_connections(&[sample("good")]).unwrap();
        // 手工拼一个含非法条目的文件（port=0 过不了域校验）
        let mut file: ConnectionsFile = serde_json::from_str(&out).unwrap();
        file.connections.push(bad);
        let raw = serde_json::to_string(&file).unwrap();

        let (valid, skipped) = parse_connections(&raw).unwrap();
        assert_eq!(valid.len(), 1);
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].contains("bad"));
    }

    #[test]
    fn non_json_rejected() {
        assert!(parse_connections("not json").is_err());
    }

    #[test]
    fn encrypted_roundtrip_and_wrong_passphrase() {
        let out = encrypt_connections_with(&[sample("enc")], "p@ss", 1_000).unwrap();
        assert!(matches!(
            prepare_import(out.clone()).unwrap(),
            PreparedImport::Encrypted(_)
        ));
        let (valid, skipped) = decrypt_connections(&out, "p@ss").unwrap();
        assert_eq!(valid.len(), 1);
        assert!(skipped.is_empty());
        assert_eq!(valid[0].password, "secret");
        assert!(
            decrypt_connections(&out, "wrong")
                .unwrap_err()
                .contains("口令")
        );
    }

    #[test]
    fn hostile_kdf_iterations_rejected() {
        let out = encrypt_connections_with(&[sample("enc")], "p", 1_000).unwrap();
        let mut file: EncryptedConnectionsFile = serde_json::from_str(&out).unwrap();
        file.kdf_iterations = 4_000_000_000;
        let raw = serde_json::to_string(&file).unwrap();
        assert!(decrypt_connections(&raw, "p").unwrap_err().contains("迭代"));
    }

    #[test]
    fn plain_file_is_prepared_without_password_prompt() {
        let out = serialize_connections(&[sample("a")]).unwrap();
        assert!(matches!(
            prepare_import(out).unwrap(),
            PreparedImport::Plain { valid, skipped }
                if valid.len() == 1 && skipped.is_empty()
        ));
    }

    #[test]
    fn duplicate_ids_are_skipped_instead_of_silently_overwritten() {
        let first = sample("first");
        let mut duplicate = first.clone();
        duplicate.name = "duplicate".into();
        let raw = serialize_connections(&[first, duplicate]).unwrap();

        let (valid, skipped) = parse_connections(&raw).unwrap();
        assert_eq!(valid.len(), 1);
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].contains("ID 重复"));
    }

    #[test]
    fn excessive_connection_count_is_rejected_during_deserialization() {
        let connections = (0..=MAX_CONNECTION_CONFIGS)
            .map(|index| sample(&format!("conn-{index}")))
            .collect();
        let raw = serde_json::to_string(&ConnectionsFile {
            version: EXPORT_VERSION,
            connections,
        })
        .unwrap();

        assert!(parse_connections(&raw).unwrap_err().contains("连接数量"));
    }

    #[test]
    fn invalid_salt_length_is_rejected() {
        let out = encrypt_connections_with(&[sample("enc")], "p", 1_000).unwrap();
        let mut file: EncryptedConnectionsFile = serde_json::from_str(&out).unwrap();
        file.kdf_salt = "00".into();
        let raw = serde_json::to_string(&file).unwrap();
        assert!(decrypt_connections(&raw, "p").unwrap_err().contains("salt"));
    }

    #[test]
    fn export_requires_nontrivial_bounded_passphrase() {
        assert!(validate_export_passphrase("short").is_err());
        assert!(validate_export_passphrase("long-enough").is_ok());
        assert!(
            validate_export_passphrase(&"x".repeat(MAX_TRANSFER_PASSPHRASE_BYTES + 1)).is_err()
        );
    }
}
