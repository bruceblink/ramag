//! 根据连接配置创建 SQLite 文件连接池。

use std::time::Duration;

use ramag_domain::entities::{ConnectionConfig, DriverKind};
use ramag_domain::error::{DomainError, Result};
use sqlx::ConnectOptions;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use tracing::warn;

use ramag_infra_sql_shared::errors::map_sqlx_common;

/// SQLite 使用单连接池，保证同一个文件上的事务和写操作顺序清晰，并减少锁竞争。
pub async fn build_pool(config: &ConnectionConfig) -> Result<SqlitePool> {
    config.validate().map_err(DomainError::InvalidConfig)?;
    if config.driver != DriverKind::Sqlite {
        return Err(DomainError::InvalidConfig(format!(
            "SqliteDriver 不支持 {:?} 类型连接",
            config.driver
        )));
    }

    let options = SqliteConnectOptions::new()
        .filename(&config.host)
        .create_if_missing(true)
        .foreign_keys(true)
        .disable_statement_logging();

    SqlitePoolOptions::new()
        .max_connections(1)
        .min_connections(0)
        .acquire_timeout(Duration::from_secs(10))
        .idle_timeout(Some(Duration::from_secs(60 * 5)))
        .test_before_acquire(true)
        .connect_with(options)
        .await
        .map_err(|error| {
            warn!(
                operation = "sql_pool_create",
                error = %error,
                path = %config.host,
                "build sqlite pool failed"
            );
            map_sqlite_error(&error)
        })
}

fn map_sqlite_error(error: &sqlx::Error) -> DomainError {
    if let Some(database_error) = error.as_database_error() {
        return DomainError::QueryFailed(format!("SQLite 错误：{}", database_error.message()));
    }
    map_sqlx_common(error)
}

pub(crate) fn map_sqlite_error_for_metadata(error: &sqlx::Error) -> DomainError {
    map_sqlite_error(error)
}
