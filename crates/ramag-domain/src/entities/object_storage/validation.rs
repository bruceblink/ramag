use super::{
    CloudProvider, MAX_OBJECT_STORAGE_BUCKET_NAME_BYTES, MAX_OBJECT_STORAGE_KEY_BYTES,
    MAX_OBJECT_STORAGE_REGION_BYTES, validate_protocol_text, validate_required_single_line,
};

pub fn validate_bucket_name_for_provider(
    provider: CloudProvider,
    value: &str,
) -> Result<(), String> {
    validate_bucket_name(value)?;
    if provider == CloudProvider::AliyunOss && value.len() > 63 {
        return Err("阿里云 OSS Bucket 名称不能超过 63 个 ASCII 字符".into());
    }
    Ok(())
}

pub fn validate_bucket_name(value: &str) -> Result<(), String> {
    validate_required_single_line("Bucket 名称", value, MAX_OBJECT_STORAGE_BUCKET_NAME_BYTES)?;
    if value.len() < 3
        || !value.is_ascii()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || !value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        return Err(
            "Bucket 名称至少 3 个字符，只能包含小写字母、数字和连字符，且首尾必须是字母或数字"
                .into(),
        );
    }
    Ok(())
}

pub fn validate_region(value: &str) -> Result<(), String> {
    validate_required_single_line("Region", value, MAX_OBJECT_STORAGE_REGION_BYTES)?;
    if !value.is_ascii()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || !value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        return Err("Region 只能包含小写字母、数字和连字符，且首尾必须是字母或数字".into());
    }
    Ok(())
}

pub fn validate_object_key(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("Object Key 不能为空".into());
    }
    validate_protocol_text("Object Key", value, MAX_OBJECT_STORAGE_KEY_BYTES)
}

pub fn validate_prefix(value: &str) -> Result<(), String> {
    validate_protocol_text("Prefix", value, MAX_OBJECT_STORAGE_KEY_BYTES)
}

pub fn validate_root_prefix(value: &str) -> Result<(), String> {
    validate_prefix(value)?;
    if value.is_empty() || value.starts_with('/') || !value.ends_with('/') {
        return Err("Root Prefix 必须是非空、相对且以 / 结尾的前缀".into());
    }
    if !is_opendal_safe_path(value.trim_end_matches('/'), false) {
        return Err("Root Prefix 含 OpenDAL 无法无损处理的字符或路径段".into());
    }
    Ok(())
}

pub fn is_opendal_safe_key(value: &str) -> bool {
    validate_object_key(value).is_ok() && is_opendal_safe_path(value, false)
}

pub fn is_opendal_safe_prefix(value: &str) -> bool {
    validate_prefix(value).is_ok()
        && (value.is_empty()
            || (value.ends_with('/') && is_opendal_safe_path(value.trim_end_matches('/'), true)))
}

pub fn is_opendal_safe_list_prefix(value: &str) -> bool {
    validate_prefix(value).is_ok()
        && (value.is_empty()
            || is_opendal_safe_path(value.strip_suffix('/').unwrap_or(value), true))
}

pub fn validate_object_name_prefix(value: &str) -> Result<(), String> {
    validate_prefix(value)?;
    if value.contains('/') || value.trim() != value || value.chars().any(is_unsafe_key_character) {
        return Err("名称前缀不能包含 /、首尾空白、控制字符或双向文本控制符".into());
    }
    Ok(())
}

fn is_opendal_safe_path(value: &str, allow_empty: bool) -> bool {
    if (!allow_empty && value.is_empty())
        || value.starts_with('/')
        || value.ends_with('/')
        || value.trim() != value
        || value.contains("//")
        || value.chars().any(is_unsafe_key_character)
    {
        return false;
    }
    !value.split('/').any(|part| matches!(part, "." | ".."))
}

fn is_unsafe_key_character(value: char) -> bool {
    value.is_control()
        || matches!(
            value,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}
