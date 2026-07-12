//! 按 ConnectionConfig 构造 PgPool。缓存在 sql-shared::PoolCache

use std::time::Duration;

use ramag_domain::entities::{ConnectionConfig, DriverKind};
use ramag_domain::error::{DomainError, Result};
use sqlx::ConnectOptions;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use tracing::warn;

use crate::errors::map_postgres_error;

/// 服务端 `pg_stat_activity` 识别 ramag 连接用
const DEFAULT_APPLICATION_NAME: &str = "ramag";

/// PG 必须指定 database，空时返回 InvalidConfig
pub async fn build_pool(config: &ConnectionConfig) -> Result<PgPool> {
    if config.driver != DriverKind::Postgres {
        return Err(DomainError::InvalidConfig(format!(
            "PostgresDriver 不支持 {:?} 类型连接",
            config.driver
        )));
    }
    let database = config
        .database
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            DomainError::InvalidConfig("PostgreSQL 必须指定具体数据库（database 字段必填）".into())
        })?;

    // SSH 隧道：启用时先确保系统 ssh 转发就绪，DB 改连 127.0.0.1:本地端口；
    // 就绪探测是阻塞调用，经 spawn_blocking 隔离，不占 tokio worker
    let cfg_for_tunnel = config.clone();
    let tunnel = tokio::task::spawn_blocking(move || ramag_infra_tunnel::ensure(&cfg_for_tunnel))
        .await
        .map_err(|e| DomainError::QueryFailed(format!("SSH 隧道任务失败：{e}")))??;
    let (host, port) = match tunnel {
        Some((h, p)) => (h, p),
        None => (config.host.clone(), config.port),
    };

    let opts = PgConnectOptions::new()
        .host(&host)
        .port(port)
        .username(&config.username)
        .password(&config.password)
        .database(database)
        .application_name(DEFAULT_APPLICATION_NAME)
        .log_statements(tracing::log::LevelFilter::Debug)
        .log_slow_statements(tracing::log::LevelFilter::Warn, Duration::from_secs(1));

    // TLS：关闭时保持历史行为（Prefer 有则用、无则降级）；
    // 开启则强制加密，配了自定义 CA 再升级为按该 CA 校验证书链（自签场景）
    let opts = match (config.tls, config.ca_cert_path.as_deref()) {
        (false, _) => opts.ssl_mode(PgSslMode::Prefer),
        (true, None) => opts.ssl_mode(PgSslMode::Require),
        (true, Some(ca)) => opts.ssl_mode(PgSslMode::VerifyCa).ssl_root_cert(ca),
    };

    PgPoolOptions::new()
        .max_connections(8)
        .min_connections(0)
        .acquire_timeout(Duration::from_secs(10))
        .idle_timeout(Some(Duration::from_secs(60 * 5)))
        .test_before_acquire(true)
        .connect_with(opts)
        .await
        .map_err(|e| {
            warn!(error = %e, host = %config.host, "build postgres pool failed");
            map_postgres_error(&e)
        })
}
