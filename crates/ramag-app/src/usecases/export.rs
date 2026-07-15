//! 结果集导出：CSV / JSON / Markdown 文本，供 UI 写文件或复制剪贴板

use std::collections::BTreeMap;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ramag_domain::entities::{QueryResult, Value};
use ramag_domain::error::{DomainError, Result};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};

static EXPORT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
                tracing::warn!(error = %error, path = %backup.display(), "remove export backup failed");
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
            tracing::warn!(error = %error, path = %path.display(), "remove export temp failed")
        }
    }
}

/// 流式导出 CSV。NULL=空字段，BLOB=hex；逗号、引号和换行按 RFC 4180 转义。
pub fn write_csv(writer: &mut dyn Write, result: &QueryResult) -> Result<()> {
    write_csv_view(writer, result, None, None)
}

/// 按源行 / 源列索引流式导出 CSV；None 表示全部，不复制 QueryResult 或单元格。
pub fn write_csv_view(
    writer: &mut dyn Write,
    result: &QueryResult,
    row_indices: Option<&[usize]>,
    column_indices: Option<&[usize]>,
) -> Result<()> {
    let mut first = true;
    for column_index in selected_indices(column_indices, result.columns.len()) {
        let Some(column) = result.columns.get(column_index) else {
            continue;
        };
        if !first {
            writer.write_all(b",").map_err(csv_write_error)?;
        }
        write_csv_text(writer, column).map_err(csv_write_error)?;
        first = false;
    }
    writer.write_all(b"\n").map_err(csv_write_error)?;

    for row_index in selected_indices(row_indices, result.rows.len()) {
        let Some(row) = result.rows.get(row_index) else {
            continue;
        };
        let mut first = true;
        for column_index in selected_indices(column_indices, result.columns.len()) {
            if result.columns.get(column_index).is_none() {
                continue;
            }
            if !first {
                writer.write_all(b",").map_err(csv_write_error)?;
            }
            match row.values.get(column_index) {
                Some(value) => write_csv_value(writer, value).map_err(csv_write_error)?,
                None => write_csv_value(writer, &Value::Null).map_err(csv_write_error)?,
            }
            first = false;
        }
        writer.write_all(b"\n").map_err(csv_write_error)?;
    }
    Ok(())
}

/// 流式导出 pretty JSON 数组，每行一个对象。
pub fn write_json(writer: &mut dyn Write, result: &QueryResult) -> Result<()> {
    write_json_view(writer, result, None, None)
}

/// 按源行 / 源列索引流式导出 JSON；None 表示全部。
pub fn write_json_view(
    writer: &mut dyn Write,
    result: &QueryResult,
    row_indices: Option<&[usize]>,
    column_indices: Option<&[usize]>,
) -> Result<()> {
    serde_json::to_writer_pretty(
        writer,
        &ExportRows {
            result,
            row_indices,
            column_indices,
        },
    )
    .map_err(|error| DomainError::Storage(format!("写入 JSON 导出内容失败：{error}")))
}

/// 流式导出 GFM 表格。单元格转义：`|`→`\|`、`\`→`\\`、换行→空格。
pub fn write_markdown(writer: &mut dyn Write, result: &QueryResult) -> Result<()> {
    write_markdown_view(writer, result, None, None)
}

/// 按源行 / 源列索引流式导出 Markdown；None 表示全部。
pub fn write_markdown_view(
    writer: &mut dyn Write,
    result: &QueryResult,
    row_indices: Option<&[usize]>,
    column_indices: Option<&[usize]>,
) -> Result<()> {
    writer.write_all(b"| ").map_err(markdown_write_error)?;
    let mut first = true;
    let mut visible_columns = 0;
    for column_index in selected_indices(column_indices, result.columns.len()) {
        let Some(column) = result.columns.get(column_index) else {
            continue;
        };
        if !first {
            writer.write_all(b" | ").map_err(markdown_write_error)?;
        }
        write_markdown_text(writer, column, false).map_err(markdown_write_error)?;
        first = false;
        visible_columns += 1;
    }
    writer.write_all(b" |\n| ").map_err(markdown_write_error)?;
    for index in 0..visible_columns {
        if index > 0 {
            writer.write_all(b" | ").map_err(markdown_write_error)?;
        }
        writer.write_all(b"---").map_err(markdown_write_error)?;
    }
    writer.write_all(b" |").map_err(markdown_write_error)?;

    for row_index in selected_indices(row_indices, result.rows.len()) {
        let Some(row) = result.rows.get(row_index) else {
            continue;
        };
        writer.write_all(b"\n| ").map_err(markdown_write_error)?;
        let mut first = true;
        for column_index in selected_indices(column_indices, result.columns.len()) {
            if result.columns.get(column_index).is_none() {
                continue;
            }
            if !first {
                writer.write_all(b" | ").map_err(markdown_write_error)?;
            }
            match row.values.get(column_index) {
                Some(value) => write_markdown_value(writer, value).map_err(markdown_write_error)?,
                None => write_markdown_value(writer, &Value::Null).map_err(markdown_write_error)?,
            }
            first = false;
        }
        writer.write_all(b" |").map_err(markdown_write_error)?;
    }
    Ok(())
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

fn csv_write_error(error: std::io::Error) -> DomainError {
    DomainError::Storage(format!("写入 CSV 导出内容失败：{error}"))
}

fn markdown_write_error(error: std::io::Error) -> DomainError {
    DomainError::Storage(format!("写入 Markdown 导出内容失败：{error}"))
}

fn write_csv_text(writer: &mut dyn Write, value: &str) -> std::io::Result<()> {
    if !value.contains([',', '"', '\n', '\r']) {
        return writer.write_all(value.as_bytes());
    }

    writer.write_all(b"\"")?;
    let bytes = value.as_bytes();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'"' {
            writer.write_all(&bytes[start..index])?;
            writer.write_all(b"\"\"")?;
            start = index + 1;
        }
    }
    writer.write_all(&bytes[start..])?;
    writer.write_all(b"\"")
}

fn write_csv_value(writer: &mut dyn Write, value: &Value) -> std::io::Result<()> {
    match value {
        Value::Null => Ok(()),
        Value::Bool(value) => write!(writer, "{value}"),
        Value::Int(value) => write!(writer, "{value}"),
        Value::Float(value) => write!(writer, "{value}"),
        Value::Text(value) => write_csv_text(writer, value),
        Value::Bytes(value) => {
            for byte in value {
                write!(writer, "{byte:02x}")?;
            }
            Ok(())
        }
        Value::DateTime(value) => write_csv_text(writer, &value.to_rfc3339()),
        Value::Json(value) => write_csv_text(writer, &value.to_string()),
    }
}

fn write_markdown_text(
    writer: &mut dyn Write,
    value: &str,
    carriage_return_as_space: bool,
) -> std::io::Result<()> {
    let bytes = value.as_bytes();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        let replacement: Option<&[u8]> = match *byte {
            b'\\' => Some(b"\\\\"),
            b'|' => Some(b"\\|"),
            b'\n' => Some(b" "),
            b'\r' if carriage_return_as_space => Some(b" "),
            b'\r' => Some(b""),
            _ => None,
        };
        if let Some(replacement) = replacement {
            writer.write_all(&bytes[start..index])?;
            writer.write_all(replacement)?;
            start = index + 1;
        }
    }
    writer.write_all(&bytes[start..])
}

fn write_markdown_value(writer: &mut dyn Write, value: &Value) -> std::io::Result<()> {
    match value {
        Value::Null => writer.write_all(b"NULL"),
        Value::Bool(value) => write!(writer, "{value}"),
        Value::Int(value) => write!(writer, "{value}"),
        Value::Float(value) => write!(writer, "{value}"),
        Value::Text(value) => write_markdown_text(writer, value, true),
        Value::Bytes(value) => write!(writer, "[{} bytes]", value.len()),
        Value::DateTime(value) => write_markdown_text(writer, &value.to_rfc3339(), false),
        Value::Json(value) => write_markdown_text(writer, &value.to_string(), false),
    }
}

struct ExportRows<'a> {
    result: &'a QueryResult,
    row_indices: Option<&'a [usize]>,
    column_indices: Option<&'a [usize]>,
}

impl Serialize for ExportRows<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(None)?;
        for row_index in selected_indices(self.row_indices, self.result.rows.len()) {
            let Some(row) = self.result.rows.get(row_index) else {
                continue;
            };
            sequence.serialize_element(&ExportRow {
                result: self.result,
                values: &row.values,
                column_indices: self.column_indices,
            })?;
        }
        sequence.end()
    }
}

struct ExportRow<'a> {
    result: &'a QueryResult,
    values: &'a [Value],
    column_indices: Option<&'a [usize]>,
}

impl Serialize for ExportRow<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // 与原 serde_json::Map 语义一致：键排序，重复列名保留最后一个值。
        let mut fields = BTreeMap::new();
        for column_index in selected_indices(self.column_indices, self.result.columns.len()) {
            let Some(column) = self.result.columns.get(column_index) else {
                continue;
            };
            fields.insert(
                column.as_str(),
                self.values
                    .get(column_index)
                    .map_or(ExportValue::Null, ExportValue::Value),
            );
        }
        let mut map = serializer.serialize_map(Some(fields.len()))?;
        for (column, value) in fields {
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
mod tests {
    use super::*;
    use ramag_domain::entities::Row;

    fn sample_result() -> QueryResult {
        QueryResult {
            columns: vec!["id".into(), "name".into(), "data".into()],
            column_types: Vec::new(),
            rows: vec![
                Row {
                    values: vec![Value::Int(1), Value::Text("张三".into()), Value::Null],
                },
                Row {
                    values: vec![
                        Value::Int(2),
                        Value::Text("李, 四".into()),
                        Value::Text("\"escaped\"".into()),
                    ],
                },
            ],
            affected_rows: 0,
            elapsed_ms: 5,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn csv_basic() {
        let mut output = Vec::new();
        write_csv(&mut output, &sample_result()).unwrap();
        let csv = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "id,name,data");
        assert_eq!(lines[1], "1,张三,");
        assert!(lines[2].contains("\"李, 四\""));
        assert!(lines[2].contains("\"\"\"escaped\"\"\""));
    }

    #[test]
    fn json_basic() {
        let mut output = Vec::new();
        write_json(&mut output, &sample_result()).unwrap();
        let json = String::from_utf8(output).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], 1);
        assert_eq!(arr[0]["name"], "张三");
        assert!(arr[0]["data"].is_null());
        assert_eq!(arr[1]["name"], "李, 四");
    }

    #[test]
    fn indexed_view_preserves_row_order_and_column_projection() {
        let result = sample_result();
        let rows = [1, 0];
        let columns = [1, 0];
        let mut csv_output = Vec::new();
        let mut json_output = Vec::new();

        write_csv_view(&mut csv_output, &result, Some(&rows), Some(&columns)).unwrap();
        write_json_view(&mut json_output, &result, Some(&rows), Some(&columns)).unwrap();

        let csv = String::from_utf8(csv_output).unwrap();
        assert!(csv.starts_with("name,id\n\"李, 四\",2\n张三,1\n"));
        let json: serde_json::Value = serde_json::from_slice(&json_output).unwrap();
        let rows = json.as_array().unwrap();
        assert_eq!(rows[0]["id"], 2);
        assert_eq!(rows[0]["name"], "李, 四");
        assert!(rows[0].get("data").is_none());
        assert_eq!(rows[1]["id"], 1);
    }

    #[test]
    fn indexed_view_writes_null_for_missing_selected_value() {
        let mut result = sample_result();
        result.rows[0].values.truncate(1);
        let rows = [0];
        let columns = [0, 2];
        let mut json_output = Vec::new();
        let mut markdown_output = Vec::new();

        write_json_view(&mut json_output, &result, Some(&rows), Some(&columns)).unwrap();
        write_markdown_view(&mut markdown_output, &result, Some(&rows), Some(&columns)).unwrap();

        let json: serde_json::Value = serde_json::from_slice(&json_output).unwrap();
        assert!(json[0]["data"].is_null());
        let markdown = String::from_utf8(markdown_output).unwrap();
        assert!(markdown.contains("| 1 | NULL |"));
    }

    #[test]
    fn markdown_streaming_preserves_escaping() {
        let mut result = sample_result();
        result.columns[1] = "na|me".into();
        result.rows[0].values[1] = Value::Text("a\\b\r\nc|d".into());
        let mut output = Vec::new();

        write_markdown(&mut output, &result).unwrap();

        let markdown = String::from_utf8(output).unwrap();
        assert!(markdown.starts_with("| id | na\\|me | data |\n"));
        assert!(markdown.contains("| 1 | a\\\\b  c\\|d | NULL |"));
        assert!(!markdown.ends_with('\n'));
    }

    #[test]
    fn atomic_write_replaces_complete_file_without_temp_remnant() -> std::io::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "ramag-export-test-{}-{}",
            std::process::id(),
            EXPORT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir)?;
        let path = dir.join("result.csv");
        std::fs::write(&path, "old")?;

        write_atomic(&path, "new content").map_err(std::io::Error::other)?;

        assert_eq!(std::fs::read_to_string(&path)?, "new content");
        assert_eq!(std::fs::read_dir(&dir)?.count(), 1);
        std::fs::remove_dir_all(dir)
    }

    #[test]
    fn failed_streaming_export_preserves_original_and_removes_temp() -> std::io::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "ramag-export-failure-test-{}-{}",
            std::process::id(),
            EXPORT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir)?;
        let path = dir.join("result.csv");
        std::fs::write(&path, "old")?;

        let result = write_atomic_with(&path, |writer| {
            writer
                .write_all(b"partial")
                .map_err(|error| DomainError::Storage(error.to_string()))?;
            Err(DomainError::Storage("test export failure".into()))
        });

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path)?, "old");
        assert_eq!(std::fs::read_dir(&dir)?.count(), 1);
        std::fs::remove_dir_all(dir)
    }

    #[cfg(unix)]
    #[test]
    fn new_atomic_export_is_private() -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join(format!(
            "ramag-export-mode-test-{}-{}",
            std::process::id(),
            EXPORT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir)?;
        let path = dir.join("result.json");

        write_atomic(&path, "[]").map_err(std::io::Error::other)?;

        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(dir)
    }
}
