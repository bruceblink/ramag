//! JumpServer 登录连接的本地加密存储与认证。

use ramag_domain::entities::{JumpServerConnection, JumpServerCredential, JumpServerSession};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::JumpServerDriver;
use tracing::{error, info, warn};

use super::{
    JUMPSERVER_CONNECTIONS_PREFERENCE_KEY, LEGACY_CREDENTIAL_PREFERENCE_KEY,
    MAX_JUMPSERVER_CONNECTIONS, SshService, deduplicate_connections, same_connection_identity,
    validate_connections,
};

impl SshService {
    pub fn with_jumpserver_driver(mut self, driver: std::sync::Arc<dyn JumpServerDriver>) -> Self {
        self.jumpserver_driver = Some(driver);
        self
    }

    pub async fn authenticate_jumpserver(
        &self,
        credential: &JumpServerCredential,
    ) -> Result<JumpServerSession> {
        credential.validate().map_err(DomainError::InvalidConfig)?;
        let started = std::time::Instant::now();
        let result = self.jumpserver_driver()?.authenticate(credential).await;
        match &result {
            Ok(session) => info!(
                operation = "jumpserver_authenticate",
                organizations = session.organizations.len(),
                elapsed_ms = started.elapsed().as_millis(),
                "jumpserver authentication succeeded"
            ),
            Err(auth_error) => {
                warn!(
                    operation = "jumpserver_authenticate",
                    error = %auth_error,
                    elapsed_ms = started.elapsed().as_millis(),
                    "jumpserver authentication failed"
                )
            }
        }
        result
    }

    /// 读取本机加密保存的 JumpServer 连接，并自动迁移旧版单连接数据。
    pub async fn load_jumpserver_connections(&self) -> Result<Vec<JumpServerConnection>> {
        let result = self.load_jumpserver_connections_inner().await;
        if let Err(error) = &result {
            warn!(
                operation = "jumpserver_connection_load",
                error = %error,
                "load jumpserver connections failed"
            );
        }
        result
    }

    async fn load_jumpserver_connections_inner(&self) -> Result<Vec<JumpServerConnection>> {
        if let Some(stored) = self
            .storage
            .get_preference(JUMPSERVER_CONNECTIONS_PREFERENCE_KEY)
            .await?
        {
            let connections: Vec<JumpServerConnection> =
                self.decrypt_jumpserver_value(&stored).await?;
            let original_len = connections.len();
            let connections = deduplicate_connections(connections);
            validate_connections(&connections)?;
            if connections.len() != original_len {
                self.store_jumpserver_connections(&connections).await?;
                info!(
                    operation = "jumpserver_connection_repair",
                    before = original_len,
                    after = connections.len(),
                    "duplicate jumpserver connections repaired"
                );
            }
            info!(
                operation = "jumpserver_connection_load",
                count = connections.len(),
                "jumpserver connections loaded"
            );
            return Ok(connections);
        }

        let Some(stored) = self
            .storage
            .get_preference(LEGACY_CREDENTIAL_PREFERENCE_KEY)
            .await?
        else {
            return Ok(Vec::new());
        };
        let credential: JumpServerCredential = self.decrypt_jumpserver_value(&stored).await?;
        credential.validate().map_err(|error| {
            DomainError::Storage(format!("已保存的 JumpServer 登录信息无效：{error}"))
        })?;
        let connections = vec![JumpServerConnection::new(credential)];
        self.store_jumpserver_connections(&connections).await?;
        self.storage
            .delete_preference(LEGACY_CREDENTIAL_PREFERENCE_KEY)
            .await?;
        info!(
            operation = "jumpserver_connection_migrate",
            count = connections.len(),
            "legacy jumpserver connection migrated"
        );
        Ok(connections)
    }

    /// 新建或更新连接；整份连接列表使用本机主密钥加密后保存。
    pub async fn save_jumpserver_connection(
        &self,
        connection_id: Option<&str>,
        credential: &JumpServerCredential,
    ) -> Result<JumpServerConnection> {
        let requested_connection_id = connection_id.unwrap_or("-");
        let result = async {
            credential.validate().map_err(DomainError::InvalidConfig)?;
            let mut connections = self.load_jumpserver_connections_inner().await?;
            let connection = if let Some(connection_id) = connection_id {
                let index = connections
                    .iter()
                    .position(|connection| connection.id == connection_id)
                    .ok_or_else(|| {
                        DomainError::NotFound("选中的 JumpServer 连接已不存在".into())
                    })?;
                let mut connection = connections.remove(index);
                connection.credential = credential.clone();
                connection
            } else if let Some(index) = connections
                .iter()
                .position(|connection| same_connection_identity(&connection.credential, credential))
            {
                let mut connection = connections.remove(index);
                connection.credential = credential.clone();
                connection
            } else {
                if connections.len() >= MAX_JUMPSERVER_CONNECTIONS {
                    return Err(DomainError::InvalidConfig(format!(
                        "JumpServer 连接最多保存 {MAX_JUMPSERVER_CONNECTIONS} 个"
                    )));
                }
                JumpServerConnection::new(credential.clone())
            };
            connections
                .retain(|item| !same_connection_identity(&item.credential, &connection.credential));
            connections.insert(0, connection.clone());
            self.store_jumpserver_connections(&connections).await?;
            Ok((connection, connections.len()))
        }
        .await;
        match &result {
            Ok((connection, total)) => info!(
                operation = "jumpserver_connection_save",
                connection_id = %connection.id,
                total,
                "jumpserver connection saved"
            ),
            Err(error) => error!(
                operation = "jumpserver_connection_save",
                connection_id = requested_connection_id,
                error = %error,
                "save jumpserver connection failed"
            ),
        }
        result.map(|(connection, _)| connection)
    }

    pub async fn delete_jumpserver_connection(&self, connection_id: &str) -> Result<()> {
        let result = async {
            let mut connections = self.load_jumpserver_connections_inner().await?;
            let previous_len = connections.len();
            connections.retain(|connection| connection.id != connection_id);
            if connections.len() == previous_len {
                return Err(DomainError::NotFound(
                    "选中的 JumpServer 连接已不存在".into(),
                ));
            }
            if connections.is_empty() {
                self.storage
                    .delete_preference(JUMPSERVER_CONNECTIONS_PREFERENCE_KEY)
                    .await?;
            } else {
                self.store_jumpserver_connections(&connections).await?;
            }
            Ok(connections.len())
        }
        .await;
        match &result {
            Ok(remaining) => info!(
                operation = "jumpserver_connection_delete",
                connection_id, remaining, "jumpserver connection deleted"
            ),
            Err(error) => {
                error!(
                    operation = "jumpserver_connection_delete",
                    connection_id,
                    error = %error,
                    "delete jumpserver connection failed"
                )
            }
        }
        result.map(|_| ())
    }
}
