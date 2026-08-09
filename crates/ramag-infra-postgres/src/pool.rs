//! 根据 ConnectionConfig 创建 PostgreSQL 连接池。

use std::time::Duration;

use ramag_domain::entities::{ConnectionConfig, DriverKind};
use ramag_domain::error::{DomainError, Result};
use sqlx::ConnectOptions;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use tracing::warn;

use crate::errors::map_postgres_error;

/// 供服务端在 `pg_stat_activity` 中识别 Ramag 连接。
const DEFAULT_APPLICATION_NAME: &str = "ramag";

/// PostgreSQL 必须指定数据库，否则返回配置错误。
pub async fn build_pool(config: &ConnectionConfig) -> Result<PgPool> {
    config.validate().map_err(DomainError::InvalidConfig)?;
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
    if config.tls
        && let Some(ca) = config
            .ca_cert_path
            .as_deref()
            .filter(|path| !path.is_empty())
    {
        ramag_infra_tunnel::validate_ca_certificate_file(ca)?;
    }

    // 启用 SSH 隧道时，数据库改连本地转发端口。
    // 隧道探测是阻塞调用，放在线程池中执行。
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
        // 查询正文可能含敏感字面量；耗时和结果规模由共享执行层单独记录。
        .disable_statement_logging();

    // 未启用 TLS 时保留优先尝试加密的历史行为；启用时按验证等级配置。
    // SSH 隧道连接本地主机，无法校验原主机名，因此完整验证降级为证书验证。
    let opts = if !config.tls {
        opts.ssl_mode(PgSslMode::Prefer)
    } else {
        let mut verify = config.tls_verify;
        if config.ssh_target.is_some() && verify == ramag_domain::entities::TlsVerify::Full {
            warn!(operation = "sql_pool_tls_policy", host = %config.host, "tls verify downgraded Full->Ca over ssh tunnel");
            verify = ramag_domain::entities::TlsVerify::Ca;
        }
        let opts = match verify {
            ramag_domain::entities::TlsVerify::None => opts.ssl_mode(PgSslMode::Require),
            ramag_domain::entities::TlsVerify::Ca => opts.ssl_mode(PgSslMode::VerifyCa),
            ramag_domain::entities::TlsVerify::Full => opts.ssl_mode(PgSslMode::VerifyFull),
        };
        match config.ca_cert_path.as_deref().filter(|s| !s.is_empty()) {
            Some(ca) => opts.ssl_root_cert(ca),
            None => opts,
        }
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
            warn!(operation = "sql_pool_create", error = %e, host = %config.host, "build postgres pool failed");
            map_postgres_error(&e)
        })
}
