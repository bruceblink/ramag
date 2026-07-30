//! ID 字符串与非负整数之间的双向转换配置。

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const MAX_ID_CONVERTER_PROGRAM_BYTES: usize = 32 * 1024;
pub const MAX_CUSTOM_ID_ALPHABET_BYTES: usize = 94;

pub const BASE10_ALPHABET: &str = "0123456789";
pub const BASE16_ALPHABET: &str = "0123456789abcdef";
pub const BASE36_ALPHABET: &str = "0123456789abcdefghijklmnopqrstuvwxyz";
pub const BASE58_BITCOIN_ALPHABET: &str =
    "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
pub const BASE58_FLICKR_ALPHABET: &str =
    "123456789abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdConverterKind {
    Base10,
    Base16,
    Base36,
    Base58Bitcoin,
    #[default]
    Base58Flickr,
    CustomAlphabet,
    ExternalProgram,
}

impl IdConverterKind {
    pub const ALL: [Self; 7] = [
        Self::Base10,
        Self::Base16,
        Self::Base36,
        Self::Base58Bitcoin,
        Self::Base58Flickr,
        Self::CustomAlphabet,
        Self::ExternalProgram,
    ];

    pub fn is_external(self) -> bool {
        matches!(self, Self::ExternalProgram)
    }

    pub fn is_custom(self) -> bool {
        matches!(self, Self::CustomAlphabet)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdConverterConfig {
    #[serde(default, rename = "id_converter_kind")]
    pub kind: IdConverterKind,
    #[serde(default, rename = "id_converter_custom_alphabet")]
    pub custom_alphabet: String,
    /// 外部转换器可执行文件的绝对路径；执行时不经过 shell。
    #[serde(default, rename = "id_converter_program")]
    pub external_program: String,
}

impl IdConverterConfig {
    pub fn validate_storable(&self) -> Result<(), String> {
        if self.external_program.len() > MAX_ID_CONVERTER_PROGRAM_BYTES {
            return Err(format!(
                "ID 转换器路径超过 {} KiB 上限",
                MAX_ID_CONVERTER_PROGRAM_BYTES / 1024
            ));
        }
        if self.external_program.chars().any(char::is_control) {
            return Err("ID 转换器路径不能包含控制字符".to_string());
        }
        if self.custom_alphabet.len() > MAX_CUSTOM_ID_ALPHABET_BYTES {
            return Err(format!(
                "自定义字符表不能超过 {MAX_CUSTOM_ID_ALPHABET_BYTES} 字节"
            ));
        }
        Ok(())
    }

    pub fn validate_active(&self) -> Result<(), String> {
        self.validate_storable()?;
        match self.kind {
            IdConverterKind::CustomAlphabet => validate_custom_alphabet(&self.custom_alphabet),
            IdConverterKind::ExternalProgram => {
                if self.external_program.is_empty() {
                    return Err("请选择 ID 转换器可执行文件".to_string());
                }
                if !Path::new(&self.external_program).is_absolute() {
                    return Err("ID 转换器必须使用绝对路径".to_string());
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// 内置与自定义模式在进程内完成；外部模式由应用层负责进程边界。
    pub fn decode_local(&self, input: &str) -> Result<i64, String> {
        let (input, alphabet, case_insensitive) = match self.kind {
            IdConverterKind::Base10 => (input, BASE10_ALPHABET, false),
            IdConverterKind::Base16 => (
                input
                    .strip_prefix("0x")
                    .or_else(|| input.strip_prefix("0X"))
                    .unwrap_or(input),
                BASE16_ALPHABET,
                true,
            ),
            IdConverterKind::Base36 => (input, BASE36_ALPHABET, true),
            IdConverterKind::Base58Bitcoin => (input, BASE58_BITCOIN_ALPHABET, false),
            IdConverterKind::Base58Flickr => (input, BASE58_FLICKR_ALPHABET, false),
            IdConverterKind::CustomAlphabet => {
                validate_custom_alphabet(&self.custom_alphabet)?;
                (input, self.custom_alphabet.as_str(), false)
            }
            IdConverterKind::ExternalProgram => {
                return Err("外部 ID 转换器不能在进程内执行".to_string());
            }
        };
        decode_positional_i64(input, alphabet, case_insensitive)
    }

    /// 使用当前字符表生成规范字符串；外部模式由应用层负责进程边界。
    pub fn encode_local(&self, value: i64) -> Result<String, String> {
        let alphabet = match self.kind {
            IdConverterKind::Base10 => BASE10_ALPHABET,
            IdConverterKind::Base16 => BASE16_ALPHABET,
            IdConverterKind::Base36 => BASE36_ALPHABET,
            IdConverterKind::Base58Bitcoin => BASE58_BITCOIN_ALPHABET,
            IdConverterKind::Base58Flickr => BASE58_FLICKR_ALPHABET,
            IdConverterKind::CustomAlphabet => {
                validate_custom_alphabet(&self.custom_alphabet)?;
                self.custom_alphabet.as_str()
            }
            IdConverterKind::ExternalProgram => {
                return Err("外部 ID 转换器不能在进程内执行".to_string());
            }
        };
        encode_positional_i64(value, alphabet)
    }
}

pub fn parse_nonnegative_id_integer(input: &str) -> Result<i64, String> {
    if input.is_empty() {
        return Err("整数 ID 不能为空".to_string());
    }
    if !input.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("整数 ID 必须只包含非负十进制数字".to_string());
    }
    let value = input
        .parse::<u64>()
        .map_err(|_| "整数 ID 超出 u64 范围".to_string())?;
    i64::try_from(value).map_err(|_| "整数 ID 超出非负 i64 范围".to_string())
}

pub fn validate_custom_alphabet(alphabet: &str) -> Result<(), String> {
    validate_alphabet_characters(alphabet)?;
    if alphabet.len() < 2 {
        return Err("自定义字符表至少需要 2 个字符".to_string());
    }
    Ok(())
}

fn validate_alphabet_characters(alphabet: &str) -> Result<(), String> {
    if !alphabet.is_ascii() {
        return Err("自定义字符表只支持可见 ASCII 字符".to_string());
    }
    if alphabet.bytes().any(|byte| !(b'!'..=b'~').contains(&byte)) {
        return Err("自定义字符表只支持可见 ASCII 字符，不能包含空格".to_string());
    }
    let mut seen = HashSet::with_capacity(alphabet.len());
    if alphabet.bytes().any(|byte| !seen.insert(byte)) {
        return Err("自定义字符表不能包含重复字符".to_string());
    }
    Ok(())
}

fn decode_positional_i64(
    input: &str,
    alphabet: &str,
    case_insensitive: bool,
) -> Result<i64, String> {
    if input.is_empty() {
        return Err("ID 搜索词不能为空".to_string());
    }
    if !input.is_ascii() {
        return Err("ID 搜索词包含当前字符表不支持的字符".to_string());
    }

    let base =
        i64::try_from(alphabet.len()).map_err(|_| "ID 转换字符表长度超出支持范围".to_string())?;
    let mut value = 0_i64;
    for byte in input.bytes() {
        let normalized = if case_insensitive {
            byte.to_ascii_lowercase()
        } else {
            byte
        };
        let digit = alphabet
            .bytes()
            .position(|candidate| candidate == normalized)
            .ok_or_else(|| {
                format!(
                    "ID 搜索词包含字符表之外的字符：{}",
                    char::from(byte).escape_default()
                )
            })?;
        let digit = i64::try_from(digit).map_err(|_| "ID 转换字符位置超出支持范围".to_string())?;
        value = value
            .checked_mul(base)
            .and_then(|current| current.checked_add(digit))
            .ok_or_else(|| "ID 转换结果超出非负 i64 范围".to_string())?;
    }
    Ok(value)
}

fn encode_positional_i64(value: i64, alphabet: &str) -> Result<String, String> {
    if value < 0 {
        return Err("整数 ID 不能为负数".to_string());
    }

    let alphabet = alphabet.as_bytes();
    if alphabet.len() < 2 {
        return Err("ID 转换字符表至少需要 2 个字符".to_string());
    }
    let base =
        i64::try_from(alphabet.len()).map_err(|_| "ID 转换字符表长度超出支持范围".to_string())?;
    if value == 0 {
        return String::from_utf8(vec![alphabet[0]])
            .map_err(|_| "ID 转换字符表不是有效 UTF-8".to_string());
    }

    let mut value = value;
    let mut encoded = Vec::new();
    while value > 0 {
        let digit =
            usize::try_from(value % base).map_err(|_| "ID 转换字符位置超出支持范围".to_string())?;
        let character = alphabet
            .get(digit)
            .copied()
            .ok_or_else(|| "ID 转换字符位置超出支持范围".to_string())?;
        encoded.push(character);
        value /= base;
    }
    encoded.reverse();
    String::from_utf8(encoded).map_err(|_| "ID 转换字符表不是有效 UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_presets_decode_known_values() -> Result<(), String> {
        let mut config = IdConverterConfig {
            kind: IdConverterKind::Base10,
            ..IdConverterConfig::default()
        };
        assert_eq!(config.decode_local("123")?, 123);

        config.kind = IdConverterKind::Base16;
        assert_eq!(config.decode_local("ff")?, 255);
        assert_eq!(config.decode_local("0X1A")?, 26);

        config.kind = IdConverterKind::Base36;
        assert_eq!(config.decode_local("1Z")?, 71);

        config.kind = IdConverterKind::Base58Flickr;
        assert_eq!(config.decode_local("qwe")?, 82_489);

        config.kind = IdConverterKind::Base58Bitcoin;
        assert_eq!(config.decode_local("qwe")?, 164_641);
        Ok(())
    }

    #[test]
    fn common_presets_encode_known_values() -> Result<(), String> {
        let mut config = IdConverterConfig {
            kind: IdConverterKind::Base10,
            ..IdConverterConfig::default()
        };
        assert_eq!(config.encode_local(123)?, "123");

        config.kind = IdConverterKind::Base16;
        assert_eq!(config.encode_local(26)?, "1a");

        config.kind = IdConverterKind::Base36;
        assert_eq!(config.encode_local(71)?, "1z");

        config.kind = IdConverterKind::Base58Flickr;
        assert_eq!(config.encode_local(82_489)?, "qwe");

        config.kind = IdConverterKind::Base58Bitcoin;
        assert_eq!(config.encode_local(164_641)?, "qwe");
        Ok(())
    }

    #[test]
    fn custom_alphabet_uses_its_declared_order() -> Result<(), String> {
        let config = IdConverterConfig {
            kind: IdConverterKind::CustomAlphabet,
            custom_alphabet: "abc".into(),
            external_program: String::new(),
        };

        assert_eq!(config.decode_local("bca")?, 15);
        assert_eq!(config.encode_local(15)?, "bca");
        Ok(())
    }

    #[test]
    fn custom_alphabet_rejects_invalid_definitions() {
        assert!(validate_custom_alphabet("").is_err());
        assert!(validate_custom_alphabet("a").is_err());
        assert!(validate_custom_alphabet("aba").is_err());
        assert!(validate_custom_alphabet("a b").is_err());
        assert!(validate_custom_alphabet("甲乙").is_err());
    }

    #[test]
    fn decoder_rejects_unknown_characters_and_overflow() {
        let flickr = IdConverterConfig {
            kind: IdConverterKind::Base58Flickr,
            ..IdConverterConfig::default()
        };
        assert!(flickr.decode_local("0").is_err());
        assert!(flickr.decode_local("O").is_err());
        assert!(flickr.decode_local("雪").is_err());
        assert!(flickr.decode_local("ZZZZZZZZZZZ").is_err());
    }

    #[test]
    fn leading_zero_digit_keeps_the_numeric_value() -> Result<(), String> {
        let flickr = IdConverterConfig {
            kind: IdConverterKind::Base58Flickr,
            ..IdConverterConfig::default()
        };

        assert_eq!(flickr.decode_local("1qwe")?, 82_489);
        Ok(())
    }

    #[test]
    fn integer_input_and_encoder_reject_invalid_values() -> Result<(), String> {
        assert_eq!(parse_nonnegative_id_integer("00042")?, 42);
        assert!(parse_nonnegative_id_integer("").is_err());
        assert!(parse_nonnegative_id_integer("-1").is_err());
        assert!(parse_nonnegative_id_integer("+1").is_err());
        assert!(parse_nonnegative_id_integer("9223372036854775808").is_err());

        let flickr = IdConverterConfig::default();
        assert_eq!(flickr.encode_local(0)?, "1");
        assert!(flickr.encode_local(-1).is_err());
        Ok(())
    }
}
