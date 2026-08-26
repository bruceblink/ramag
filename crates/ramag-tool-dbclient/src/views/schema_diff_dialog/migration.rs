use std::path::PathBuf;

use gpui::{Context, px};
use gpui_component::notification::Notification;
use ramag_app::usecases::export;
use ramag_domain::entities::{DriverKind, Query};

use super::super::schema_migration::{MigrationScript, build_migration_script};
use super::SchemaDiffDialog;

#[path = "approval.rs"]
mod approval;
#[path = "render.rs"]
mod render;
pub(super) use approval::{MigrationApprovalRecord, load_migration_approvals};

enum MigrationExportOutcome {
    Saved(PathBuf),
    Cancelled,
    Failed { path: PathBuf, error: String },
}

impl SchemaDiffDialog {
    /// Rebuilds the script from the currently loaded metadata so save and execute never use a stale preview.
    fn current_migration_script(&self) -> Result<MigrationScript, String> {
        let Some((source, target)) = self.source.as_ref().zip(self.target.as_ref()) else {
            return Err("两张表的元数据尚未加载，无法生成迁移 SQL".into());
        };
        if !source.warnings.is_empty() || !target.warnings.is_empty() {
            return Err("源表或目标表的元数据未完整加载，无法执行迁移 SQL；请刷新后重试".into());
        }
        if self.source_connection.driver != self.target_connection.driver {
            return Err("源连接和目标连接必须使用相同数据库驱动，无法执行迁移 SQL".into());
        }
        build_migration_script(
            self.target_connection.driver,
            &self.source_schema,
            &self.source_table,
            &self.target_schema,
            &self.target_table,
            &source.metadata,
            &target.metadata,
        )
    }

    pub(super) fn toggle_migration(&mut self, cx: &mut Context<Self>) {
        self.migration_visible = !self.migration_visible;
        self.migration_vertical_scroll
            .set_offset(gpui::Point::new(px(0.0), px(0.0)));
        self.migration_horizontal_scroll
            .set_offset(gpui::Point::new(px(0.0), px(0.0)));
        cx.notify();
    }

    pub(super) fn save_migration_sql(&mut self, cx: &mut Context<Self>) {
        if self.saving_migration || self.executing_migration {
            return;
        }
        let script = match self.current_migration_script() {
            Ok(script) => script,
            Err(error) => {
                self.pending_notification = Some(Notification::error(error).autohide(true));
                cx.notify();
                return;
            }
        };
        if script.statement_count == 0 {
            self.pending_notification =
                Some(Notification::info("结构已一致，无需保存迁移 SQL").autohide(true));
            cx.notify();
            return;
        }

        let database_type = match self.target_connection.driver {
            ramag_domain::entities::DriverKind::Mysql => "mysql",
            ramag_domain::entities::DriverKind::Postgres => "postgresql",
            _ => "sql",
        };
        let object = format!("{}_to_{}", self.source_table, self.target_table);
        let file_name = export::suggested_export_file_name(
            database_type,
            &self.target_schema,
            Some(&object),
            false,
            "sql",
        );
        let source_name = self.source_table.clone();
        let target_name = self.target_table.clone();
        let connection_id = self.target_connection.id.to_string();
        self.saving_migration = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = match rfd::AsyncFileDialog::new()
                .set_file_name(&file_name)
                .add_filter("SQL", &["sql"])
                .save_file()
                .await
            {
                None => MigrationExportOutcome::Cancelled,
                Some(handle) => {
                    let path = handle.path().to_path_buf();
                    let write_path = path.clone();
                    match ramag_app::run_blocking(move || {
                        export::write_atomic(&write_path, &script.sql)
                    })
                    .await
                    {
                        Ok(()) => MigrationExportOutcome::Saved(path),
                        Err(error) => MigrationExportOutcome::Failed {
                            path,
                            error: error.to_string(),
                        },
                    }
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.saving_migration = false;
                this.pending_save_notification(
                    outcome,
                    &connection_id,
                    &source_name,
                    &target_name,
                    cx,
                );
            });
        })
        .detach();
    }

    /// Requests explicit approval, then executes the unchanged script against the target connection.
    pub(super) fn request_execute_migration(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving_migration || self.executing_migration {
            return;
        }
        if self.target_connection.production {
            self.pending_notification = Some(
                Notification::warning("目标连接已标记为生产环境，只能预览或保存迁移 SQL")
                    .autohide(true),
            );
            cx.notify();
            return;
        }
        let script = match self.current_migration_script() {
            Ok(script) => script,
            Err(error) => {
                self.pending_notification = Some(Notification::error(error).autohide(true));
                cx.notify();
                return;
            }
        };
        if script.statement_count == 0 {
            self.pending_notification =
                Some(Notification::info("结构已一致，无需执行迁移 SQL").autohide(true));
            cx.notify();
            return;
        }

        let transaction_note = match self.target_connection.driver {
            DriverKind::Postgres => "PostgreSQL 将在一个事务中执行，任一语句失败会回滚本次迁移。",
            DriverKind::Mysql => "MySQL DDL 可能隐式提交；执行失败时目标表可能已经部分变更。",
            _ => "当前数据库驱动不支持迁移 SQL 执行。",
        };
        let mut description = format!(
            "目标连接：{}\n目标表：{}.{}\n将执行 {} 条迁移语句，其中 {} 条删除或修改。\n{}\n此操作会直接修改目标数据库。",
            self.target_connection.name,
            self.target_schema,
            self.target_table,
            script.statement_count,
            script.destructive_statements,
            transaction_note,
        );
        if !script.warnings.is_empty() {
            description.push_str("\n生成提示：");
            for warning in script.warnings.iter().take(4) {
                description.push_str("\n- ");
                description.push_str(warning);
            }
        }

        let entity = cx.entity();
        let request_generation = self.request_generation;
        let target_connection_id = self.target_connection.id.clone();
        let source_schema = self.source_schema.clone();
        let source_table = self.source_table.clone();
        let target_schema = self.target_schema.clone();
        let target_table = self.target_table.clone();
        let expected_sql = script.sql.clone();
        ramag_ui::open_confirm(
            "执行迁移 SQL？",
            description,
            "确认执行",
            script.destructive_statements > 0,
            move |_, app| {
                entity.update(app, |this, cx| {
                    let context_changed = this.request_generation != request_generation
                        || this.target_connection.id != target_connection_id
                        || this.source_schema != source_schema
                        || this.source_table != source_table
                        || this.target_schema != target_schema
                        || this.target_table != target_table
                        || this
                            .current_migration_script()
                            .map_or(true, |current| current.sql != expected_sql);
                    if context_changed {
                        this.pending_notification = Some(
                            Notification::warning("迁移预览已变化，请重新打开预览并确认")
                                .autohide(true),
                        );
                        cx.notify();
                        return;
                    }
                    let approval_id = approval::append_migration_approval(this, &script, cx);
                    this.start_migration_execution(script, approval_id, cx);
                });
            },
            window,
            cx,
        );
    }

    /// Runs the approved script once and reloads both sides so the dialog reflects the database.
    fn start_migration_execution(
        &mut self,
        script: MigrationScript,
        approval_id: String,
        cx: &mut Context<Self>,
    ) {
        let execution_generation = self.migration_execution_generation.wrapping_add(1);
        self.migration_execution_generation = execution_generation;
        self.executing_migration = true;
        let service = self.service.clone();
        let target_connection = self.target_connection.clone();
        let target_schema = self.target_schema.clone();
        let target_table = self.target_table.clone();
        let query = migration_query(target_connection.driver, &target_schema, script.sql.clone());
        let statement_count = script.statement_count;
        let destructive_statements = script.destructive_statements;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = service.execute(&target_connection, &query).await;
            let _ = this.update(cx, |this, cx| {
                if this.migration_execution_generation != execution_generation {
                    return;
                }
                this.executing_migration = false;
                match result {
                    Ok(result) => {
                        let warning_count = result.warnings.len();
                        approval::record_migration_outcome(
                            this,
                            &approval_id,
                            approval::MigrationApprovalStatus::Executed,
                            Some(result.elapsed_ms),
                            warning_count,
                            None,
                            cx,
                        );
                        let warning_suffix = if warning_count == 0 {
                            String::new()
                        } else {
                            format!("，数据库返回 {warning_count} 条警告")
                        };
                        tracing::info!(
                            operation = "schema_migration_execute",
                            connection_id = %target_connection.id,
                            target_schema = %target_schema,
                            target_table = %target_table,
                            statements = statement_count,
                            destructive_statements,
                            elapsed_ms = result.elapsed_ms,
                            warning_count,
                            "schema migration executed"
                        );
                        this.pending_notification = Some(
                            Notification::success(format!(
                                "迁移执行成功：{statement_count} 条语句，耗时 {} ms{warning_suffix}；正在重新读取元数据",
                                result.elapsed_ms
                            ))
                            .autohide(true),
                        );
                        this.refresh(cx);
                    }
                    Err(error) => {
                        let error_text = error.to_string();
                        approval::record_migration_outcome(
                            this,
                            &approval_id,
                            approval::MigrationApprovalStatus::Failed,
                            None,
                            0,
                            Some(&error_text),
                            cx,
                        );
                        tracing::error!(
                            operation = "schema_migration_execute",
                            connection_id = %target_connection.id,
                            target_schema = %target_schema,
                            target_table = %target_table,
                            statements = statement_count,
                            destructive_statements,
                            error = %error_text,
                            "schema migration execution failed"
                        );
                        this.pending_notification =
                            Some(Notification::error(format!("迁移执行失败：{error_text}")).autohide(true));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn pending_save_notification(
        &mut self,
        outcome: MigrationExportOutcome,
        connection_id: &str,
        source_table: &str,
        target_table: &str,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            MigrationExportOutcome::Cancelled => return,
            MigrationExportOutcome::Saved(path) => {
                tracing::info!(
                    operation = "schema_migration_export",
                    connection_id,
                    source_table,
                    target_table,
                    path = %path.display(),
                    "schema migration script exported"
                );
                let file_name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.pending_notification = Some(
                    Notification::success(format!("已保存迁移 SQL：{file_name}")).autohide(true),
                );
            }
            MigrationExportOutcome::Failed { path, error } => {
                tracing::error!(
                    operation = "schema_migration_export",
                    connection_id,
                    source_table,
                    target_table,
                    path = %path.display(),
                    error = %error,
                    "schema migration script export failed"
                );
                self.pending_notification =
                    Some(Notification::error(format!("保存迁移 SQL 失败：{error}")).autohide(true));
            }
        }
        cx.notify();
    }
}

/// Builds the execution query with the transaction rule supported by each SQL dialect.
fn migration_query(driver: DriverKind, schema: &str, sql: String) -> Query {
    let query = Query::new(sql).with_schema(schema);
    if driver == DriverKind::Postgres {
        query.transactional()
    } else {
        query
    }
}

#[cfg(test)]
mod tests {
    use super::migration_query;
    use ramag_domain::entities::DriverKind;

    #[test]
    fn postgres_migration_query_is_transactional() {
        let query = migration_query(DriverKind::Postgres, "public", "SELECT 1".into());
        assert!(query.transactional);
        assert_eq!(query.default_schema.as_deref(), Some("public"));
    }

    #[test]
    fn mysql_migration_query_keeps_ddl_autocommit_behavior() {
        let query = migration_query(DriverKind::Mysql, "app", "SELECT 1".into());
        assert!(!query.transactional);
        assert_eq!(query.default_schema.as_deref(), Some("app"));
    }
}
