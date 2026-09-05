//! SQLite 驱动：以本地数据库文件为连接目标，复用 SQL 共享执行与事务实现。

pub mod metadata;
pub mod pool;
pub mod types;

use async_trait::async_trait;

use ramag_domain::entities::{
    Column, ConnectionConfig, DriverKind, ForeignKey, Index, Schema, Table, Trigger, Value,
};
use ramag_domain::error::Result;
use ramag_infra_sql_shared::sql::SplitOptions;
use ramag_infra_sql_shared::{PoolCache, SqlBackend, TransactionStore};
use sqlx::sqlite::{Sqlite, SqlitePool, SqliteQueryResult, SqliteRow};
use sqlx::{Column as _, Row as _, TypeInfo as _};

/// SQLite 不提供跨连接的远程查询取消；查询仍可使用共享执行器，但取消按钮会明确报不支持。
#[derive(Clone, Default)]
pub struct SqliteDriver {
    pools: PoolCache<Sqlite>,
    transactions: TransactionStore<Sqlite>,
}

impl SqliteDriver {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SqlBackend for SqliteDriver {
    type Db = Sqlite;

    fn name(&self) -> &'static str {
        "sqlite"
    }

    fn driver_kind(&self) -> DriverKind {
        DriverKind::Sqlite
    }

    fn cache(&self) -> &PoolCache<Self::Db> {
        &self.pools
    }

    fn transaction_store(&self) -> &TransactionStore<Self::Db> {
        &self.transactions
    }

    fn quote_identifier(&self, ident: &str) -> String {
        DriverKind::Sqlite.quote_identifier(ident)
    }

    fn supports_query_cancellation(&self) -> bool {
        false
    }

    fn cancel_query_sql(&self, _backend_id: u64) -> String {
        String::new()
    }

    fn use_database_sql(&self, _db: &str) -> Option<String> {
        None
    }

    fn split_options(&self) -> SplitOptions {
        SplitOptions::sqlite()
    }

    async fn build_pool(&self, config: &ConnectionConfig) -> Result<SqlitePool> {
        pool::build_pool(config).await
    }

    fn decode_row(&self, row: &SqliteRow) -> Result<Vec<Value>> {
        types::decode_row(row)
    }

    fn extract_columns(&self, row: &SqliteRow) -> (Vec<String>, Vec<String>) {
        row.columns()
            .iter()
            .map(|column| {
                (
                    column.name().to_string(),
                    column.type_info().name().to_string(),
                )
            })
            .unzip()
    }

    fn rows_affected(&self, result: &SqliteQueryResult) -> u64 {
        result.rows_affected()
    }

    async fn server_version_impl(&self, pool: &SqlitePool) -> Result<String> {
        metadata::server_version(pool).await
    }

    async fn list_schemas_impl(&self, pool: &SqlitePool) -> Result<Vec<Schema>> {
        metadata::list_schemas(pool).await
    }

    async fn list_tables_impl(&self, pool: &SqlitePool, schema: &str) -> Result<Vec<Table>> {
        metadata::list_tables(pool, schema).await
    }

    async fn list_columns_impl(
        &self,
        pool: &SqlitePool,
        schema: &str,
        table: &str,
    ) -> Result<Vec<Column>> {
        metadata::list_columns(pool, schema, table).await
    }

    async fn list_indexes_impl(
        &self,
        pool: &SqlitePool,
        schema: &str,
        table: &str,
    ) -> Result<Vec<Index>> {
        metadata::list_indexes(pool, schema, table).await
    }

    async fn list_foreign_keys_impl(
        &self,
        pool: &SqlitePool,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ForeignKey>> {
        metadata::list_foreign_keys(pool, schema, table).await
    }

    async fn list_triggers_impl(
        &self,
        pool: &SqlitePool,
        schema: &str,
        table: &str,
    ) -> Result<Vec<Trigger>> {
        metadata::list_triggers(pool, schema, table).await
    }
}

ramag_infra_sql_shared::impl_driver_for!(SqliteDriver);

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::{Query, Value, build_ddl_query};
    use ramag_domain::traits::Driver;

    #[tokio::test]
    async fn sqlite_driver_round_trips_queries_and_metadata()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("client.sqlite3");
        let config = ConnectionConfig::new_sqlite("local", path.to_string_lossy());
        let driver = SqliteDriver::new();

        driver.test_connection(&config).await?;
        driver
            .execute(
                &config,
                &Query::new(
                    "CREATE TABLE teams (id INTEGER PRIMARY KEY); CREATE TABLE users (id INTEGER PRIMARY KEY, team_id INTEGER, name TEXT NOT NULL, active BOOLEAN, payload JSON, FOREIGN KEY(team_id) REFERENCES teams(id) ON DELETE CASCADE ON UPDATE SET NULL); CREATE UNIQUE INDEX idx_users_name ON users(name); CREATE TRIGGER users_audit AFTER INSERT ON users BEGIN UPDATE teams SET id = id WHERE id = NEW.team_id; END; CREATE VIEW user_names AS SELECT name FROM users; INSERT INTO teams (id) VALUES (1); INSERT INTO users (team_id, name, active, payload) VALUES (1, 'Alice', TRUE, '{\"role\":\"admin\"}');",
                )
                .transactional(),
            )
            .await?;

        let result = driver
            .execute(&config, &Query::new("SELECT id, name FROM users"))
            .await?;
        assert_eq!(result.columns, ["id", "name"]);
        assert!(matches!(result.rows[0].values[0], Value::Int(1)));
        assert!(matches!(&result.rows[0].values[1], Value::Text(name) if name == "Alice"));

        let version = driver.server_version(&config).await?;
        assert!(!version.is_empty());
        let schemas = driver.list_schemas(&config).await?;
        assert!(schemas.iter().any(|schema| schema.name == "main"));
        let tables = driver.list_tables(&config, "main").await?;
        assert!(tables.iter().any(|table| table.name == "users"));
        assert!(
            tables
                .iter()
                .any(|table| table.name == "user_names" && table.is_view)
        );
        let columns = driver.list_columns(&config, "main", "users").await?;
        assert_eq!(columns.len(), 5);
        assert!(columns[0].is_primary_key);
        assert!(columns.iter().any(|column| column.name == "payload"));

        let indexes = driver.list_indexes(&config, "main", "users").await?;
        assert!(indexes.iter().any(|index| {
            index.name == "idx_users_name" && index.unique && index.columns == ["name"]
        }));
        let foreign_keys = driver.list_foreign_keys(&config, "main", "users").await?;
        assert!(foreign_keys.iter().any(|foreign_key| {
            foreign_key.ref_table == "teams"
                && foreign_key.columns == ["team_id"]
                && foreign_key.ref_columns == ["id"]
                && foreign_key.on_delete == ramag_domain::entities::ForeignKeyAction::Cascade
                && foreign_key.on_update == ramag_domain::entities::ForeignKeyAction::SetNull
        }));
        let triggers = driver.list_triggers(&config, "main", "users").await?;
        assert!(triggers.iter().any(|trigger| {
            trigger.name == "users_audit"
                && trigger.timing == "AFTER"
                && trigger.event == "INSERT"
                && trigger.definition.contains("UPDATE teams")
        }));

        let ddl = driver
            .execute(
                &config,
                &Query::new(build_ddl_query(DriverKind::Sqlite, "main", "users", false)),
            )
            .await?;
        assert!(
            matches!(&ddl.rows[0].values[0], Value::Text(sql) if sql.contains("CREATE TABLE users"))
        );

        driver
            .execute(
                &config,
                &Query::new("ALTER TABLE users ADD COLUMN email TEXT; ALTER TABLE users RENAME COLUMN email TO email_address;"),
            )
            .await?;
        let altered_columns = driver.list_columns(&config, "main", "users").await?;
        assert!(
            altered_columns
                .iter()
                .any(|column| column.name == "email_address")
        );
        Ok(())
    }

    #[tokio::test]
    async fn sqlite_manual_transactions_support_savepoints_and_cleanup()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("transactions.sqlite3");
        let config = ConnectionConfig::new_sqlite("local", path.to_string_lossy());
        let driver = SqliteDriver::new();

        let transaction = driver.begin_transaction(&config).await?;
        driver
            .execute_in_transaction(
                &config,
                &transaction,
                &Query::new(
                    "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL); INSERT INTO items (name) VALUES ('first');",
                ),
            )
            .await?;
        driver
            .create_savepoint(&config, &transaction, "before_second")
            .await?;
        driver
            .execute_in_transaction(
                &config,
                &transaction,
                &Query::new("INSERT INTO items (name) VALUES ('second');"),
            )
            .await?;
        driver
            .rollback_to_savepoint(&config, &transaction, "before_second")
            .await?;
        driver
            .release_savepoint(&config, &transaction, "before_second")
            .await?;
        driver
            .execute_in_transaction(
                &config,
                &transaction,
                &Query::new("INSERT INTO items (name) VALUES ('third');"),
            )
            .await?;
        driver.commit_transaction(&config, &transaction).await?;

        let result = driver
            .execute(&config, &Query::new("SELECT name FROM items ORDER BY id"))
            .await?;
        assert_eq!(result.rows.len(), 2);
        assert!(matches!(&result.rows[0].values[0], Value::Text(name) if name == "first"));
        assert!(matches!(&result.rows[1].values[0], Value::Text(name) if name == "third"));

        let rolled_back = driver.begin_transaction(&config).await?;
        driver
            .execute_in_transaction(
                &config,
                &rolled_back,
                &Query::new("INSERT INTO items (name) VALUES ('discarded');"),
            )
            .await?;
        driver.rollback_transaction(&config, &rolled_back).await?;
        let result = driver
            .execute(&config, &Query::new("SELECT COUNT(*) FROM items"))
            .await?;
        assert!(matches!(result.rows[0].values[0], Value::Int(2)));
        Ok(())
    }

    #[tokio::test]
    async fn sqlite_rejects_direct_transaction_controls_and_rolls_back_failed_batch()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("guard.sqlite3");
        let config = ConnectionConfig::new_sqlite("local", path.to_string_lossy());
        let driver = SqliteDriver::new();

        let error = driver
            .execute(&config, &Query::new("BEGIN"))
            .await
            .err()
            .ok_or_else(|| std::io::Error::other("BEGIN should be rejected"))?;
        assert!(error.message().contains("事务控制按钮"));

        let error = driver
            .execute(
                &config,
                &Query::new(
                    "CREATE TABLE rollback_probe (id INTEGER); INSERT INTO rollback_probe VALUES (1); INSERT INTO missing_table VALUES (1);",
                )
                .transactional(),
            )
            .await
            .err()
            .ok_or_else(|| std::io::Error::other("failed batch should be rolled back"))?;
        assert!(error.message().contains("missing_table"));
        assert!(
            driver
                .execute(&config, &Query::new("SELECT * FROM rollback_probe"))
                .await
                .is_err()
        );
        Ok(())
    }
}
