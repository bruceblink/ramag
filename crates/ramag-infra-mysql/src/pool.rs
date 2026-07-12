//! 按 ConnectionConfig 构造 MySqlPool。缓存逻辑在 sql-shared::PoolCache

use std::time::Duration;

use ramag_domain::entities::{ConnectionConfig, DriverKind};
use ramag_domain::error::{DomainError, Result};
use sqlx::ConnectOptions;
use sqlx::MySqlPool;
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlSslMode};
use tracing::warn;

use crate::errors::map_mysql_error;

pub async fn build_pool(config: &ConnectionConfig) -> Result<MySqlPool> {
    if config.driver != DriverKind::Mysql {
        return Err(DomainError::InvalidConfig(format!(
            "MysqlDriver 不支持 {:?} 类型连接",
            config.driver
        )));
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
        // utf8mb4 覆盖 emoji + 全部中文
        .charset("utf8mb4")
        // 统一 UTC 避免时区歧义
        .timezone(Some("+00:00".into()))
        .log_statements(tracing::log::LevelFilter::Debug)
        .log_slow_statements(tracing::log::LevelFilter::Warn, Duration::from_secs(1));

    // TLS：关闭时保持历史行为（Preferred 有则用、无则降级）；
    // 开启则强制加密，配了自定义 CA 再升级为按该 CA 校验证书链（自签场景）
    let opts = match (config.tls, config.ca_cert_path.as_deref()) {
        (false, _) => opts.ssl_mode(MySqlSslMode::Preferred),
        (true, None) => opts.ssl_mode(MySqlSslMode::Required),
        (true, Some(ca)) => opts.ssl_mode(MySqlSslMode::VerifyCa).ssl_ca(ca),
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
