//! 系统凭据库：macOS 使用 Keychain，Windows 使用 Credential Manager，Linux 使用 Secret Service。
//! 32 字节主密钥仅存系统凭据库，不落盘；redb 文件被拷走也无法解密。

use ramag_domain::error::{DomainError, Result};
use rand::TryRngCore;
use tracing::info;

const SERVICE: &str = "ramag";
const ACCOUNT: &str = "master-key";
const KEY_LEN: usize = 32;

/// 首次随机生成并写入系统凭据库；已有数据库时禁止静默重建丢失的密钥。
pub fn get_or_create_master_key(allow_create: bool) -> Result<[u8; KEY_LEN]> {
    let entry = keyring::Entry::new(SERVICE, ACCOUNT)
        .map_err(|e| DomainError::Storage(format!("初始化系统凭据库失败：{e}")))?;

    match entry.get_password() {
        Ok(hex_str) => {
            let bytes = hex::decode(hex_str.trim())
                .map_err(|e| DomainError::Storage(format!("系统凭据库主密钥格式错误：{e}")))?;
            if bytes.len() != KEY_LEN {
                // 不能静默重建：旧 redb 仍由原密钥加密，覆盖后会永久失去解密能力。
                return Err(DomainError::Storage(format!(
                    "系统凭据库主密钥长度错误：应为 {KEY_LEN} 字节，实际为 {} 字节",
                    bytes.len()
                )));
            }
            let mut key = [0u8; KEY_LEN];
            key.copy_from_slice(&bytes);
            Ok(key)
        }
        Err(keyring::Error::NoEntry) if allow_create => {
            info!("master key not found, generating new one");
            generate_and_save(&entry)
        }
        Err(keyring::Error::NoEntry) => Err(DomainError::Storage(
            "检测到已有加密数据库，但系统凭据库缺少主密钥；为避免覆盖恢复线索，已停止启动。请先恢复系统凭据，或备份并移走旧数据库后重试".into(),
        )),
        Err(e) => Err(DomainError::Storage(format!("读取系统凭据库失败：{e}"))),
    }
}

fn generate_and_save(entry: &keyring::Entry) -> Result<[u8; KEY_LEN]> {
    let mut key = [0u8; KEY_LEN];
    rand::rngs::OsRng
        .try_fill_bytes(&mut key)
        .map_err(|e| DomainError::Storage(format!("OS 随机源不可用：{e}")))?;
    entry
        .set_password(&hex::encode(key))
        .map_err(|e| DomainError::Storage(format!("写入系统凭据库失败：{e}")))?;
    info!("master key created and stored in credential store");
    Ok(key)
}

/// 测试 / 调试用，生产慎用：删除会让已加密数据全部无法解密
#[cfg(any(test, debug_assertions))]
pub fn delete_master_key() -> Result<()> {
    let entry = keyring::Entry::new(SERVICE, ACCOUNT)
        .map_err(|e| DomainError::Storage(format!("初始化系统凭据库失败：{e}")))?;
    match entry.delete_credential() {
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(DomainError::Storage(format!("删除系统凭据条目失败：{e}"))),
    }
}
