//! SQL 手动事务的保存点操作。

use super::ConnectionService;
use ramag_domain::entities::{ConnectionConfig, TransactionId};
use ramag_domain::error::Result;

impl ConnectionService {
    /// Creates a savepoint without closing the current SQL transaction.
    pub async fn create_savepoint(
        &self,
        config: &ConnectionConfig,
        transaction: &TransactionId,
        name: &str,
    ) -> Result<()> {
        let result = match self.driver_for(config) {
            Ok(driver) => driver.create_savepoint(config, transaction, name).await,
            Err(error) => Err(error),
        };
        super::log_connection_result(
            "sql_transaction_savepoint_create",
            config,
            None,
            None,
            &result,
        );
        result
    }

    /// Rolls back to a savepoint while keeping the surrounding transaction open.
    pub async fn rollback_to_savepoint(
        &self,
        config: &ConnectionConfig,
        transaction: &TransactionId,
        name: &str,
    ) -> Result<()> {
        let result = match self.driver_for(config) {
            Ok(driver) => {
                driver
                    .rollback_to_savepoint(config, transaction, name)
                    .await
            }
            Err(error) => Err(error),
        };
        super::log_connection_result(
            "sql_transaction_savepoint_rollback",
            config,
            None,
            None,
            &result,
        );
        result
    }

    /// Releases a savepoint while keeping the surrounding transaction open.
    pub async fn release_savepoint(
        &self,
        config: &ConnectionConfig,
        transaction: &TransactionId,
        name: &str,
    ) -> Result<()> {
        let result = match self.driver_for(config) {
            Ok(driver) => driver.release_savepoint(config, transaction, name).await,
            Err(error) => Err(error),
        };
        super::log_connection_result(
            "sql_transaction_savepoint_release",
            config,
            None,
            None,
            &result,
        );
        result
    }
}
