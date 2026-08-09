//! 将结果集导出为每行一个 JSON 对象的 JSONL 文件。

use std::collections::BTreeMap;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ramag_domain::entities::{QueryResult, Value};
use ramag_domain::error::{DomainError, Result};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};

static EXPORT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const MAX_EXPORT_FILE_SEGMENT_BYTES: usize = 64;
const MAX_EXPORT_FILE_EXTENSION_BYTES: usize = 10;

/// 统一生成用户可见的导出文件名：ramag-数据库类型-库[-对象][-data]-时间.扩展名。
/// 数据库名、表名、集合名与 Key 均来自外部系统，统一处理跨平台非法字符。
/// 和长度边界，避免保存框拿到不可用文件名。
pub fn suggested_export_file_name(
    database_type: &str,
    database: &str,
    object: Option<&str>,
    data_only: bool,
    extension: &str,
) -> String {
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    suggested_export_file_name_at(
        database_type,
        database,
        object,
        data_only,
        extension,
        &timestamp,
    )
}

fn suggested_export_file_name_at(
    database_type: &str,
    database: &str,
    object: Option<&str>,
    data_only: bool,
    extension: &str,
    timestamp: &str,
) -> String {
    let mut segments = vec![
        "ramag".to_string(),
        sanitize_export_file_segment(database_type).to_ascii_lowercase(),
        sanitize_export_file_segment(database),
    ];
    if let Some(object) = object.filter(|value| !value.trim().is_empty()) {
        segments.push(sanitize_export_file_segment(object));
    }
    if data_only {
        segments.push("data".to_string());
    }
    segments.push(sanitize_export_file_segment(timestamp));

    let stem = segments.join("-");
    let extension = extension
        .trim()
        .trim_start_matches('.')
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(MAX_EXPORT_FILE_EXTENSION_BYTES)
        .collect::<String>()
        .to_ascii_lowercase();
    if extension.is_empty() {
        stem
    } else {
        format!("{stem}.{extension}")
    }
}

fn sanitize_export_file_segment(value: &str) -> String {
    let mut output = String::with_capacity(value.len().min(MAX_EXPORT_FILE_SEGMENT_BYTES));
    let mut pending_separator = false;
    for character in value.trim().chars() {
        let invalid = character.is_control()
            || character.is_whitespace()
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            );
        if invalid {
            pending_separator = !output.is_empty();
            continue;
        }

        let separator_bytes = usize::from(pending_separator);
        if output.len() + separator_bytes + character.len_utf8() > MAX_EXPORT_FILE_SEGMENT_BYTES {
            break;
        }
        if pending_separator {
            output.push('_');
            pending_separator = false;
        }
        output.push(character);
    }
    if output.is_empty() {
        "unknown".to_string()
    } else {
        output
    }
}

/// 在目标同目录完整写入临时文件，再替换目标；失败时原文件保持不变。
pub fn write_atomic(path: &Path, content: &str) -> Result<()> {
    write_atomic_with(path, |writer| {
        writer
            .write_all(content.as_bytes())
            .map_err(|error| DomainError::Storage(format!("写入导出临时文件失败：{error}")))
    })
}

/// 流式生成同目录临时文件，完整写入后再替换目标。
pub fn write_atomic_with<F>(path: &Path, write_content: F) -> Result<()>
where
    F: FnOnce(&mut dyn Write) -> Result<()>,
{
    let file_name = path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| DomainError::InvalidConfig("导出路径缺少文件名".into()))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let existing_permissions = match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(DomainError::InvalidConfig(
                    "导出目标不能是符号链接，请选择普通文件".into(),
                ));
            }
            if !metadata.is_file() {
                return Err(DomainError::InvalidConfig(
                    "导出目标已存在且不是普通文件".into(),
                ));
            }
            Some(metadata.permissions())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(DomainError::Storage(format!(
                "检查导出目标 {} 失败：{error}",
                path.display()
            )));
        }
    };

    let (temp_path, file) = create_export_temp(parent, file_name)?;
    let result = (|| -> Result<()> {
        if let Some(permissions) = existing_permissions {
            std::fs::set_permissions(&temp_path, permissions)
                .map_err(|error| DomainError::Storage(format!("保留导出文件权限失败：{error}")))?;
        }
        let mut writer = BufWriter::with_capacity(64 * 1024, file);
        write_content(&mut writer)?;
        writer
            .flush()
            .map_err(|error| DomainError::Storage(format!("刷新导出临时文件失败：{error}")))?;
        let file = writer
            .into_inner()
            .map_err(|error| DomainError::Storage(format!("完成导出临时文件写入失败：{error}")))?;
        file.sync_all()
            .map_err(|error| DomainError::Storage(format!("同步导出临时文件失败：{error}")))?;
        drop(file);
        commit_export_temp(&temp_path, path)
    })();
    if result.is_err() {
        remove_export_temp(&temp_path);
    }
    result
}

fn create_export_temp(
    parent: &Path,
    file_name: &std::ffi::OsStr,
) -> Result<(PathBuf, std::fs::File)> {
    for _ in 0..16 {
        let sequence = EXPORT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp_name = format!(
            ".{}.ramag-export-{}-{sequence}.tmp",
            file_name.to_string_lossy(),
            std::process::id()
        );
        let temp_path = parent.join(temp_name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(&temp_path) {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(DomainError::Storage(format!(
                    "创建导出临时文件 {} 失败：{error}",
                    temp_path.display()
                )));
            }
        }
    }
    Err(DomainError::Storage("无法生成唯一的导出临时文件名".into()))
}

#[cfg(not(target_os = "windows"))]
fn commit_export_temp(temp_path: &Path, target: &Path) -> Result<()> {
    std::fs::rename(temp_path, target).map_err(|error| {
        DomainError::Storage(format!("替换导出文件 {} 失败：{error}", target.display()))
    })
}

#[cfg(target_os = "windows")]
fn commit_export_temp(temp_path: &Path, target: &Path) -> Result<()> {
    let exists = match std::fs::symlink_metadata(target) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(DomainError::Storage(format!(
                "检查导出目标 {} 失败：{error}",
                target.display()
            )));
        }
    };
    if !exists {
        return std::fs::rename(temp_path, target).map_err(|error| {
            DomainError::Storage(format!("保存导出文件 {} 失败：{error}", target.display()))
        });
    }

    let backup = target.with_extension(format!(
        "ramag-backup-{}-{}",
        std::process::id(),
        EXPORT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::rename(target, &backup).map_err(|error| {
        DomainError::Storage(format!("备份原导出文件 {} 失败：{error}", target.display()))
    })?;
    match std::fs::rename(temp_path, target) {
        Ok(()) => {
            if let Err(error) = std::fs::remove_file(&backup) {
                tracing::warn!(
                    operation = "export_cleanup",
                    error = %error,
                    path = %backup.display(),
                    stage = "backup",
                    "remove export backup failed"
                );
            }
            Ok(())
        }
        Err(error) => {
            let restore = std::fs::rename(&backup, target).err();
            Err(DomainError::Storage(format!(
                "替换导出文件 {} 失败：{error}{}",
                target.display(),
                restore.map_or_else(String::new, |restore| format!(
                    "；恢复原文件失败：{restore}"
                ))
            )))
        }
    }
}

fn remove_export_temp(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(
                operation = "export_cleanup",
                error = %error,
                path = %path.display(),
                stage = "temporary_file",
                "remove export temp failed"
            )
        }
    }
}

/// 流式导出 JSONL：每行一个紧凑 JSON 对象。
pub fn write_jsonl(writer: &mut dyn Write, result: &QueryResult) -> Result<()> {
    write_jsonl_view(writer, result, None, None)
}

/// 按行列索引流式导出 JSONL；`None` 表示全部。
pub fn write_jsonl_view(
    writer: &mut dyn Write,
    result: &QueryResult,
    row_indices: Option<&[usize]>,
    column_indices: Option<&[usize]>,
) -> Result<()> {
    // 列名排序与重复列“后者覆盖前者”的映射只依赖表头，不能在每一行重复分配 BTreeMap。
    let projection = json_projection(result, column_indices);
    for row_index in selected_indices(row_indices, result.rows.len()) {
        let Some(row) = result.rows.get(row_index) else {
            continue;
        };
        serde_json::to_writer(
            &mut *writer,
            &ExportRow {
                values: &row.values,
                projection: &projection,
            },
        )
        .map_err(|error| DomainError::Storage(format!("写入 JSONL 导出内容失败：{error}")))?;
        writer
            .write_all(b"\n")
            .map_err(|error| DomainError::Storage(format!("写入 JSONL 换行失败：{error}")))?;
    }
    Ok(())
}

fn json_projection<'a>(
    result: &'a QueryResult,
    column_indices: Option<&[usize]>,
) -> Vec<(&'a str, usize)> {
    let mut fields = BTreeMap::new();
    for column_index in selected_indices(column_indices, result.columns.len()) {
        if let Some(column) = result.columns.get(column_index) {
            fields.insert(column.as_str(), column_index);
        }
    }
    fields.into_iter().collect()
}

enum SelectedIndices<'a> {
    All(std::ops::Range<usize>),
    Selected(std::iter::Copied<std::slice::Iter<'a, usize>>),
}

impl Iterator for SelectedIndices<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            SelectedIndices::All(indices) => indices.next(),
            SelectedIndices::Selected(indices) => indices.next(),
        }
    }
}

fn selected_indices(indices: Option<&[usize]>, len: usize) -> SelectedIndices<'_> {
    match indices {
        Some(indices) => SelectedIndices::Selected(indices.iter().copied()),
        None => SelectedIndices::All(0..len),
    }
}

struct ExportRow<'result, 'projection> {
    values: &'result [Value],
    projection: &'projection [(&'result str, usize)],
}

impl Serialize for ExportRow<'_, '_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.projection.len()))?;
        for &(column, column_index) in self.projection {
            let value = self
                .values
                .get(column_index)
                .map_or(ExportValue::Null, ExportValue::Value);
            map.serialize_entry(column, &value)?;
        }
        map.end()
    }
}

#[derive(Clone, Copy)]
enum ExportValue<'a> {
    Null,
    Value(&'a Value),
}

impl Serialize for ExportValue<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            ExportValue::Null | ExportValue::Value(Value::Null) => serializer.serialize_none(),
            ExportValue::Value(Value::Bool(value)) => serializer.serialize_bool(*value),
            ExportValue::Value(Value::Int(value)) => serializer.serialize_i64(*value),
            ExportValue::Value(Value::Float(value)) if value.is_finite() => {
                serializer.serialize_f64(*value)
            }
            ExportValue::Value(Value::Float(_)) => serializer.serialize_none(),
            ExportValue::Value(Value::Text(value)) => serializer.serialize_str(value),
            ExportValue::Value(Value::Bytes(value)) => {
                serializer.serialize_str(&hex::encode(value))
            }
            ExportValue::Value(Value::DateTime(value)) => {
                serializer.serialize_str(&value.to_rfc3339())
            }
            ExportValue::Value(Value::Json(value)) => value.serialize(serializer),
        }
    }
}

#[cfg(test)]
mod tests;
