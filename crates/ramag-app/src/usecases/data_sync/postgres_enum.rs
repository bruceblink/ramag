//! PostgreSQL 枚举元数据：规范化身份、稳定签名和兼容性校验。

use std::collections::BTreeMap;

use ramag_domain::entities::{ConnectionConfig, Query, QueryResult, Value};
use ramag_domain::error::{DomainError, Result};

use super::service::DataSyncService;
use crate::usecases::transfer::sql_catalog::pg_enum_types_query;

pub(super) struct PostgresRawEnum {
    pub create_statement: String,
    pub signature: String,
}

pub(super) struct PostgresEnumStatement {
    pub name: String,
    pub create_statement: String,
}

pub(super) async fn load_postgres_enum_definitions(
    service: &DataSyncService,
    config: &ConnectionConfig,
    namespace: &str,
) -> Result<BTreeMap<String, String>> {
    let result = service
        .connection_service()
        .execute(config, &Query::new(pg_enum_types_query(namespace)))
        .await?;
    let mut definitions = BTreeMap::new();
    for item in postgres_enum_rows(&result)? {
        let parsed = postgres_enum_statement(&item.create_statement)?.ok_or_else(|| {
            DomainError::QueryFailed(format!(
                "无法解析目标 Schema {namespace} 的 PostgreSQL ENUM 定义"
            ))
        })?;
        if definitions
            .insert(parsed.name.clone(), item.signature)
            .is_some()
        {
            return Err(DomainError::QueryFailed(format!(
                "目标 Schema {namespace} 返回了重复的枚举类型 {}",
                parsed.name
            )));
        }
    }
    Ok(definitions)
}

pub(super) fn postgres_enum_rows(result: &QueryResult) -> Result<Vec<PostgresRawEnum>> {
    let mut enums = Vec::with_capacity(result.rows.len());
    for row in &result.rows {
        let (Some(Value::Text(create_statement)), Some(Value::Text(signature))) =
            (row.values.first(), row.values.get(1))
        else {
            return Err(DomainError::QueryFailed(
                "PostgreSQL ENUM 元数据结果类型异常".into(),
            ));
        };
        enums.push(PostgresRawEnum {
            create_statement: create_statement.clone(),
            signature: signature.clone(),
        });
    }
    Ok(enums)
}

pub(super) fn incompatible_postgres_enum_error(namespace: &str, name: &str) -> DomainError {
    DomainError::InvalidConfig(format!(
        "目标枚举类型 {namespace}.{name} 已存在，但选项定义与源不一致；为避免数据语义错误，已停止同步"
    ))
}

pub(super) fn postgres_enum_statement(statement: &str) -> Result<Option<PostgresEnumStatement>> {
    let Some(body) = statement.trim_start().strip_prefix("CREATE TYPE ") else {
        return Ok(None);
    };
    let (qualified_name, definition) = body
        .split_once(" AS ENUM")
        .ok_or_else(|| DomainError::QueryFailed("PostgreSQL ENUM 定义缺少 AS ENUM".into()))?;
    let name = postgres_type_name(qualified_name).ok_or_else(|| {
        DomainError::QueryFailed(format!(
            "无法解析 PostgreSQL ENUM 类型名称：{}",
            qualified_name.trim()
        ))
    })?;
    let definition = definition.trim().trim_end_matches(';').trim();
    if !definition.starts_with('(') || !definition.ends_with(')') {
        return Err(DomainError::QueryFailed(format!(
            "PostgreSQL ENUM {name} 的选项定义不完整"
        )));
    }
    Ok(Some(PostgresEnumStatement {
        name,
        create_statement: statement.to_string(),
    }))
}

fn postgres_type_name(qualified_name: &str) -> Option<String> {
    let value = qualified_name.trim();
    let mut quoted = false;
    let mut separator = None;
    let mut chars = value.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch == '"' {
            if quoted && chars.peek().is_some_and(|(_, next)| *next == '"') {
                chars.next();
            } else {
                quoted = !quoted;
            }
        } else if ch == '.' && !quoted {
            if separator.is_some() {
                return None;
            }
            separator = Some(index);
        }
    }
    if quoted {
        return None;
    }
    let separator = separator?;
    decode_postgres_identifier(value.get(..separator)?.trim())?;
    decode_postgres_identifier(value.get(separator + 1..)?.trim())
}

fn decode_postgres_identifier(value: &str) -> Option<String> {
    if let Some(body) = value
        .strip_prefix('"')
        .and_then(|item| item.strip_suffix('"'))
    {
        return (!body.is_empty()).then(|| body.replace("\"\"", "\""));
    }
    let mut chars = value.chars();
    let first = chars.next()?;
    if !(first == '_' || first.is_alphabetic())
        || !chars.all(|ch| ch == '_' || ch == '$' || ch.is_alphanumeric())
    {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_statement_normalizes_quoted_and_plain_names() {
        let quoted = postgres_enum_statement(
            "CREATE TYPE \"target\".\"record_state\" AS ENUM ('new', 'in progress');",
        )
        .expect("解析带引号枚举")
        .expect("应识别枚举语句");
        let plain = postgres_enum_statement(
            "CREATE TYPE target.record_state AS ENUM ('new', 'in progress');",
        )
        .expect("解析裸标识符枚举")
        .expect("应识别枚举语句");

        assert_eq!(quoted.name, "record_state");
        assert_eq!(quoted.name, plain.name);
    }

    #[test]
    fn enum_statement_rejects_incomplete_or_invalid_names() {
        assert!(postgres_enum_statement("CREATE TYPE public.status ('ready');").is_err());
        assert!(
            postgres_enum_statement("CREATE TYPE public.123status AS ENUM ('ready');").is_err()
        );
        assert!(
            postgres_enum_statement("CREATE TABLE public.items (id BIGINT);")
                .expect("非枚举语句无需报错")
                .is_none()
        );
    }
}
