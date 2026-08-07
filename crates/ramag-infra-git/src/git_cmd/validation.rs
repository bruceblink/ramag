use super::*;

/// 用户可编辑的 Git 名称（branch/tag/remote）：禁止被解析成选项或包含不可见分隔字符。
pub(crate) fn validate_name_arg(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_GIT_NAME_ARG_BYTES
        || value.starts_with('-')
        || value.chars().any(char::is_whitespace)
        || value.chars().any(char::is_control)
    {
        return Err(DomainError::InvalidConfig(format!(
            "{label}为空、过长，或包含空白/控制字符/前导 '-'"
        )));
    }
    Ok(())
}

/// revision / URL 等单个位置参数可含普通空格，但不能成为 Git 选项或携带控制字符。
pub(crate) fn validate_positional_arg(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_GIT_POSITIONAL_ARG_BYTES
        || value.starts_with('-')
        || value.chars().any(char::is_control)
    {
        return Err(DomainError::InvalidConfig(format!(
            "{label}为空、过长，或包含控制字符/前导 '-'"
        )));
    }
    Ok(())
}

/// 路径位于 `--` 或 `--pathspec-from-file` 后，可合法以 `-` 开头或包含空白与换行。
pub(crate) fn validate_path_arg(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_GIT_PATH_BYTES || value.contains('\0') {
        return Err(DomainError::InvalidConfig(format!(
            "{label}为空、超过 {} KiB，或包含 NUL 字符",
            MAX_GIT_PATH_BYTES / 1024
        )));
    }
    let depth = value.bytes().filter(|byte| *byte == b'/').count() + 1;
    if depth > MAX_GIT_PATH_DEPTH {
        return Err(DomainError::InvalidConfig(format!(
            "{label}超过 {MAX_GIT_PATH_DEPTH} 层目录上限"
        )));
    }
    Ok(())
}

pub(crate) fn validate_path_args(paths: &[String], label: &str) -> Result<()> {
    encode_pathspec_size(paths, label).map(|_| ())
}

pub(crate) fn validate_output_path(value: &str, label: &str, index: usize) -> Result<()> {
    validate_path_arg(value, label).map_err(|error| {
        DomainError::QueryFailed(format!(
            "{label}第 {index} 条超出安全边界：{}",
            error.message()
        ))
    })
}

pub(super) fn encode_pathspecs(paths: &[String]) -> Result<String> {
    let capacity = encode_pathspec_size(paths, "Git 文件路径列表")?;
    let mut encoded = String::with_capacity(capacity);
    for path in paths {
        encoded.push_str(path);
        encoded.push('\0');
    }
    Ok(encoded)
}

pub(super) fn encode_pathspec_size(paths: &[String], label: &str) -> Result<usize> {
    if paths.is_empty() {
        return Err(DomainError::InvalidConfig(format!("{label}不能为空")));
    }
    if paths.len() > MAX_GIT_PATH_ARGS {
        return Err(DomainError::InvalidConfig(format!(
            "{label}超过 {MAX_GIT_PATH_ARGS} 条安全上限"
        )));
    }
    let mut total = 0usize;
    for path in paths {
        validate_path_arg(path, "Git 文件路径")?;
        total = total
            .checked_add(path.len().saturating_add(1))
            .ok_or_else(|| DomainError::InvalidConfig(format!("{label}总长度溢出")))?;
        if total > MAX_GIT_PATH_ARGS_BYTES {
            return Err(DomainError::InvalidConfig(format!(
                "{label}总长度超过 {} MiB 安全上限",
                MAX_GIT_PATH_ARGS_BYTES / 1024 / 1024
            )));
        }
    }
    Ok(total)
}

/// 子进程输出有字节上限，但解析成大量 String/实体后仍会放大；在分配下一项前拦截。
pub(crate) fn ensure_git_list_room(current_len: usize, label: &str) -> Result<()> {
    if current_len >= MAX_PARSED_GIT_ITEMS {
        return Err(DomainError::QueryFailed(format!(
            "{label}超过 {MAX_PARSED_GIT_ITEMS} 条安全上限，请缩小仓库或操作范围"
        )));
    }
    Ok(())
}

/// 单条路径/记录异常大时，避免 UTF-8 转换和实体复制继续放大内存。
pub(crate) fn ensure_git_record_size(bytes: &[u8], label: &str, index: usize) -> Result<()> {
    ensure_git_size(bytes, MAX_GIT_RECORD_BYTES, label, index)
}

/// commit 正文可明显大于普通路径/列表行，但仍需阻止单条 64 MiB 输出再次整段复制。
pub(crate) fn ensure_git_message_size(bytes: &[u8], label: &str, index: usize) -> Result<()> {
    ensure_git_size(bytes, MAX_GIT_MESSAGE_BYTES, label, index)
}

pub(super) fn ensure_git_size(bytes: &[u8], limit: usize, label: &str, index: usize) -> Result<()> {
    if bytes.len() > limit {
        return Err(DomainError::QueryFailed(format!(
            "{label}第 {index} 条超过 {} KiB 安全上限",
            limit / 1024
        )));
    }
    Ok(())
}
