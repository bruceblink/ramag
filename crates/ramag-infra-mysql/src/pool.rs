//! 根据连接配置构造 MySQL 连接池。

use std::time::Duration;

use ramag_domain::entities::{ConnectionConfig, DriverKind};
use ramag_domain::error::{DomainError, Result};
use sqlx::ConnectOptions;
use sqlx::MySqlPool;
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlSslMode};
use tracing::warn;

use crate::errors::map_mysql_error;

pub async fn build_pool(config: &ConnectionConfig) -> Result<MySqlPool> {
    config.validate().map_err(DomainError::InvalidConfig)?;
    if config.driver != DriverKind::Mysql {
        return Err(DomainError::InvalidConfig(format!(
            "MysqlDriver 不支持 {:?} 类型连接",
            config.driver
        )));
    }
    if config.tls
        && let Some(ca) = config
            .ca_cert_path
            .as_deref()
            .filter(|path| !path.is_empty())
    {
        ramag_infra_tunnel::validate_ca_certificate_file(ca)?;
    }

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

    let opts = MySqlConnectOptions::new()
        .host(&host)
        .port(port)
        .username(&config.username)
        .password(&config.password)
        // utf8mb4 覆盖 emoji 和完整 Unicode 字符。
        .charset("utf8mb4")
        // 统一使用 UTC，避免时区歧义。
        .timezone(Some("+00:00".into()))
        // 查询正文可能含敏感字面量；耗时和结果规模由共享执行层单独记录。
        .disable_statement_logging();

    // TLS：关闭保持历史行为（Preferred 有则用、无则降级）；开启按验证等级三档映射。
    // 经 SSH 隧道时 Full 降级为 Ca（实际连 127.0.0.1，主机名校验必败；隧道本身已加密）
    let opts = if !config.tls {
        opts.ssl_mode(MySqlSslMode::Preferred)
    } else {
        let mut verify = config.tls_verify;
        if config.ssh_target.is_some() && verify == ramag_domain::entities::TlsVerify::Full {
            warn!(host = %config.host, from = "full", to = "ca", "TLS verification downgraded over SSH tunnel");
            verify = ramag_domain::entities::TlsVerify::Ca;
        }
        let opts = match verify {
            ramag_domain::entities::TlsVerify::None => opts.ssl_mode(MySqlSslMode::Required),
            ramag_domain::entities::TlsVerify::Ca => opts.ssl_mode(MySqlSslMode::VerifyCa),
            ramag_domain::entities::TlsVerify::Full => opts.ssl_mode(MySqlSslMode::VerifyIdentity),
        };
        match config.ca_cert_path.as_deref().filter(|s| !s.is_empty()) {
            Some(ca) => opts.ssl_ca(ca),
            None => opts,
        }
    };

    let opts = if let Some(db) = config.database.as_ref().filter(|s| !s.is_empty()) {
        opts.database(db)
    } else {
        opts
    };

    MySqlPoolOptions::new()
        .max_connections(8)
        .min_connections(0)
        .acquire_timeout(Duration::from_secs(10))
        .idle_timeout(Some(Duration::from_secs(60 * 5)))
        .test_before_acquire(true)
        .connect_with(opts)
        .await
        .map_err(|e| {
            warn!(error = %e, host = %config.host, "build mysql pool failed");
            map_mysql_error(&e)
        })
}
