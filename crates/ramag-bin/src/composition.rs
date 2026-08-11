//! 应用服务、存储和工具注册表组合。

use super::*;

/// Reqwest 选择 no-provider 模式后，进程组合根负责在构建任何客户端前安装 Provider。
pub(super) fn install_tls_crypto_provider() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        anyhow::bail!("无法安装 TLS 加密 Provider");
    }
    Ok(())
}

pub(super) fn build_connection_service()
-> anyhow::Result<(Arc<ConnectionService>, Arc<dyn Storage>)> {
    use ramag_domain::entities::DriverKind;
    use std::collections::HashMap;

    let mut drivers: HashMap<DriverKind, Arc<dyn Driver>> = HashMap::new();
    drivers.insert(DriverKind::Mysql, Arc::new(MysqlDriver::new()));
    drivers.insert(DriverKind::Postgres, Arc::new(PostgresDriver::new()));

    let storage_impl =
        RedbStorage::open_default().map_err(|e| anyhow::anyhow!("初始化 redb 存储失败: {e}"))?;
    info!(operation = "storage_open", path = %storage_impl.path().display(), "storage opened");
    let storage: Arc<dyn Storage> = Arc::new(storage_impl);

    let svc = Arc::new(ConnectionService::new(drivers, storage.clone()));
    Ok((svc, storage))
}

/// 启动期同步批量读取偏好。current-thread runtime 不创建后台工作线程；同一批 key 共用一次初始化。
pub(super) fn read_preferences(
    storage: &Arc<dyn Storage>,
    keys: &[&'static str],
) -> HashMap<&'static str, String> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            warn!(operation = "preference_runtime_init", error = %error, key_count = keys.len(), "preference runtime creation failed");
            return HashMap::new();
        }
    };
    let mut preferences = HashMap::with_capacity(keys.len());
    for &key in keys {
        match runtime.block_on(storage.get_preference(key)) {
            Ok(Some(value)) => {
                preferences.insert(key, value);
            }
            Ok(None) => {}
            Err(error) => {
                warn!(operation = "preference_load", error = %error, key, "load preference failed");
            }
        }
    }
    preferences
}

pub(super) fn build_tool_registry() -> Arc<ToolRegistry> {
    let registry = Arc::new(ToolRegistry::new());
    registry.register(Arc::new(DbClientTool::new()));
    registry.register(Arc::new(VcsTool::new()));
    registry.register(Arc::new(SshTool::new()));
    registry.register(Arc::new(ObjectStorageTool::new()));
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    registry.register(Arc::new(ClipboardTool::new()));
    registry
}

pub(super) fn build_redis_service(storage: Arc<dyn Storage>) -> Arc<RedisService> {
    let driver: Arc<dyn KvDriver> = Arc::new(RedisDriver::new());
    Arc::new(RedisService::new(driver, storage))
}

pub(super) fn build_mongo_service(storage: Arc<dyn Storage>) -> Arc<MongoService> {
    let driver: Arc<dyn DocDriver> = Arc::new(MongoDriver::new());
    Arc::new(MongoService::new(driver, storage))
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(super) fn build_clipboard_service(storage: Arc<dyn Storage>) -> Arc<ClipboardService> {
    let driver: Arc<dyn ClipboardDriver> = Arc::new(PlatformClipboardDriver::new());
    Arc::new(ClipboardService::new(driver, storage))
}

pub(super) fn build_ssh_service(storage: Arc<dyn Storage>) -> Arc<SshService> {
    let driver: Arc<dyn SshDriver> = Arc::new(OpenSshDriver::new());
    let service = SshService::new(driver, storage);
    match JumpServerHttpDriver::new() {
        Ok(driver) => {
            let driver: Arc<dyn JumpServerDriver> = Arc::new(driver);
            Arc::new(service.with_jumpserver_driver(driver))
        }
        Err(error) => {
            warn!(operation = "jumpserver_client_init", error = %error, "initialize JumpServer client failed");
            Arc::new(service)
        }
    }
}

pub(super) fn build_object_storage_service(
    storage: Arc<dyn Storage>,
) -> anyhow::Result<Arc<ramag_app::ObjectStorageService>> {
    let infra = Arc::new(
        ramag_infra_object_storage::ObjectStorageInfra::new().map_err(|error| {
            anyhow::anyhow!("初始化对象存储基础设施失败：{}", error.safe_message)
        })?,
    );
    let driver: Arc<dyn ramag_domain::traits::ObjectStorageDriver> = infra;
    Ok(Arc::new(ramag_app::ObjectStorageService::new(
        driver, storage,
    )))
}

pub(super) fn build_update_service(storage: Arc<dyn Storage>) -> Option<Arc<UpdateService>> {
    match GitHubUpdateDriver::new(env!("CARGO_PKG_VERSION")) {
        Ok(driver) => Some(Arc::new(UpdateService::new(
            Arc::new(driver),
            storage,
            env!("CARGO_PKG_VERSION"),
        ))),
        Err(error) => {
            warn!(operation = "application_update_service_init", error = %error, "initialize update service failed");
            None
        }
    }
}
