use std::collections::HashSet;

use chrono::{DateTime, Utc};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px};
use gpui_component::{Theme, h_flex, notification::Notification, v_flex};
use ramag_domain::entities::{ConnectionId, DriverKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::super::super::schema_migration::MigrationScript;
use super::super::SchemaDiffDialog;

const MIGRATION_APPROVALS_PREF: &str = "dbclient_schema_migration_approvals";
const MAX_MIGRATION_APPROVAL_RECORDS: usize = 50;
const MAX_MIGRATION_APPROVAL_PREF_BYTES: usize = 128 * 1024;
const MAX_MIGRATION_APPROVAL_TEXT_BYTES: usize = 1024;
const MAX_MIGRATION_APPROVAL_ERROR_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum MigrationApprovalStatus {
    Approved,
    Executed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MigrationApprovalRecord {
    approval_id: String,
    source_connection_id: String,
    source_connection_name: String,
    target_connection_id: String,
    target_connection_name: String,
    source_schema: String,
    source_table: String,
    target_schema: String,
    target_table: String,
    driver: DriverKind,
    statement_count: usize,
    destructive_statements: usize,
    sql_sha256: String,
    approved_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    status: MigrationApprovalStatus,
    #[serde(default)]
    elapsed_ms: Option<u64>,
    #[serde(default)]
    warning_count: usize,
    #[serde(default)]
    error: Option<String>,
}

impl MigrationApprovalRecord {
    /// Validates persisted fields before they re-enter the dialog or are written back.
    fn validate(&self) -> Result<(), String> {
        validate_text("审批记录 ID", &self.approval_id)?;
        validate_text("源连接 ID", &self.source_connection_id)?;
        validate_text("源连接名称", &self.source_connection_name)?;
        validate_text("目标连接 ID", &self.target_connection_id)?;
        validate_text("目标连接名称", &self.target_connection_name)?;
        validate_text("源 schema", &self.source_schema)?;
        validate_text("源表名", &self.source_table)?;
        validate_text("目标 schema", &self.target_schema)?;
        validate_text("目标表名", &self.target_table)?;
        if self.sql_sha256.len() != 64
            || !self.sql_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("迁移脚本指纹不是有效的 SHA-256 值".into());
        }
        if self
            .error
            .as_ref()
            .is_some_and(|error| error.len() > MAX_MIGRATION_APPROVAL_ERROR_BYTES)
        {
            return Err("迁移审批错误信息过长".into());
        }
        Ok(())
    }

    /// Trims untrusted stored error text so one preference cannot grow without a bound.
    fn enforce_limits(&mut self) {
        self.error = self
            .error
            .take()
            .map(|error| bounded_text(&error, MAX_MIGRATION_APPROVAL_ERROR_BYTES));
    }

    /// Applies one execution result and keeps the user-visible error bounded.
    fn apply_outcome(
        &mut self,
        status: MigrationApprovalStatus,
        elapsed_ms: Option<u64>,
        warning_count: usize,
        error: Option<&str>,
    ) {
        self.status = status;
        self.updated_at = Utc::now();
        self.elapsed_ms = elapsed_ms;
        self.warning_count = warning_count;
        self.error = error.map(|error| bounded_text(error, MAX_MIGRATION_APPROVAL_ERROR_BYTES));
    }
}

/// Loads the bounded local approval history without blocking the metadata dialog.
pub(crate) fn load_migration_approvals(cx: &mut Context<SchemaDiffDialog>) {
    let Some(storage) = ramag_ui::theme::storage_from_cx(cx) else {
        return;
    };
    cx.spawn(async move |this, cx| {
        let outcome = match storage.get_preference(MIGRATION_APPROVALS_PREF).await {
            Ok(None) => Ok((Vec::new(), false)),
            Ok(Some(json)) => parse_migration_approvals(&json),
            Err(error) => Err(format!("读取迁移审批记录失败：{error}")),
        };
        let _ = this.update(cx, |dialog, cx| match outcome {
            Ok((records, adjusted)) => {
                let current = std::mem::take(&mut dialog.migration_approvals);
                dialog.migration_approvals = merge_migration_approvals(records, current);
                if adjusted {
                    persist_migration_approvals(dialog, cx);
                }
                cx.notify();
            }
            Err(error) => {
                dialog.pending_notification = Some(Notification::warning(error).autohide(true));
                cx.notify();
            }
        });
    })
    .detach();
}

/// Adds an approval record before execution so a failed or interrupted run remains visible.
pub(super) fn append_migration_approval(
    dialog: &mut SchemaDiffDialog,
    script: &MigrationScript,
    cx: &mut Context<SchemaDiffDialog>,
) -> String {
    let approval_id = ConnectionId::new().to_string();
    let now = Utc::now();
    dialog.migration_approvals.insert(
        0,
        MigrationApprovalRecord {
            approval_id: approval_id.clone(),
            source_connection_id: dialog.source_connection.id.to_string(),
            source_connection_name: bounded_text(
                &dialog.source_connection.name,
                MAX_MIGRATION_APPROVAL_TEXT_BYTES,
            ),
            target_connection_id: dialog.target_connection.id.to_string(),
            target_connection_name: bounded_text(
                &dialog.target_connection.name,
                MAX_MIGRATION_APPROVAL_TEXT_BYTES,
            ),
            source_schema: bounded_text(&dialog.source_schema, MAX_MIGRATION_APPROVAL_TEXT_BYTES),
            source_table: bounded_text(&dialog.source_table, MAX_MIGRATION_APPROVAL_TEXT_BYTES),
            target_schema: bounded_text(&dialog.target_schema, MAX_MIGRATION_APPROVAL_TEXT_BYTES),
            target_table: bounded_text(&dialog.target_table, MAX_MIGRATION_APPROVAL_TEXT_BYTES),
            driver: dialog.target_connection.driver,
            statement_count: script.statement_count,
            destructive_statements: script.destructive_statements,
            sql_sha256: migration_sql_digest(&script.sql),
            approved_at: now,
            updated_at: now,
            status: MigrationApprovalStatus::Approved,
            elapsed_ms: None,
            warning_count: 0,
            error: None,
        },
    );
    dialog
        .migration_approvals
        .truncate(MAX_MIGRATION_APPROVAL_RECORDS);
    persist_migration_approvals(dialog, cx);
    cx.notify();
    approval_id
}

/// Updates the matching record with the database result while retaining its script fingerprint.
pub(super) fn record_migration_outcome(
    dialog: &mut SchemaDiffDialog,
    approval_id: &str,
    status: MigrationApprovalStatus,
    elapsed_ms: Option<u64>,
    warning_count: usize,
    error: Option<&str>,
    cx: &mut Context<SchemaDiffDialog>,
) {
    let Some(record) = dialog
        .migration_approvals
        .iter_mut()
        .find(|record| record.approval_id == approval_id)
    else {
        return;
    };
    record.apply_outcome(status, elapsed_ms, warning_count, error);
    persist_migration_approvals(dialog, cx);
    cx.notify();
}

fn persist_migration_approvals(dialog: &SchemaDiffDialog, cx: &mut Context<SchemaDiffDialog>) {
    // Persist newest records first and drop the oldest entries until the preference stays bounded.
    let mut records = dialog.migration_approvals.clone();
    loop {
        match serde_json::to_string(&records) {
            Ok(json) if json.len() <= MAX_MIGRATION_APPROVAL_PREF_BYTES => {
                ramag_ui::preferences::persist_preference_latest(
                    MIGRATION_APPROVALS_PREF,
                    json,
                    cx,
                );
                return;
            }
            Ok(_) if records.pop().is_some() => {}
            Ok(_) => {
                tracing::warn!(
                    operation = "schema_migration_approval_persist",
                    "schema migration approval records exceed storage limit"
                );
                return;
            }
            Err(error) => {
                tracing::warn!(
                    operation = "schema_migration_approval_persist",
                    error = %error,
                    "serialize schema migration approval records failed"
                );
                return;
            }
        }
    }
}

/// Parses local history, validates each record, removes duplicates, and keeps only 50 entries.
fn parse_migration_approvals(json: &str) -> Result<(Vec<MigrationApprovalRecord>, bool), String> {
    if json.len() > MAX_MIGRATION_APPROVAL_PREF_BYTES {
        return Err(format!("迁移审批记录过大：{} bytes", json.len()));
    }
    let mut records: Vec<MigrationApprovalRecord> =
        serde_json::from_str(json).map_err(|error| format!("解析迁移审批记录失败：{error}"))?;
    let original_len = records.len();
    let mut seen = HashSet::with_capacity(original_len.min(MAX_MIGRATION_APPROVAL_RECORDS));
    let mut adjusted = false;
    records.retain_mut(|record| {
        record.enforce_limits();
        if !seen.insert(record.approval_id.clone()) {
            adjusted = true;
            return false;
        }
        true
    });
    if records.len() != original_len {
        adjusted = true;
    }
    if records.len() > MAX_MIGRATION_APPROVAL_RECORDS {
        records.truncate(MAX_MIGRATION_APPROVAL_RECORDS);
        adjusted = true;
    }
    for record in &records {
        record.validate()?;
    }
    records.sort_by_key(|record| std::cmp::Reverse(record.updated_at));
    Ok((records, adjusted))
}

fn merge_migration_approvals(
    loaded: Vec<MigrationApprovalRecord>,
    current: Vec<MigrationApprovalRecord>,
) -> Vec<MigrationApprovalRecord> {
    // Keep an approval created while storage was loading and let the current record win duplicates.
    let mut seen = HashSet::new();
    let mut merged =
        Vec::with_capacity((loaded.len() + current.len()).min(MAX_MIGRATION_APPROVAL_RECORDS));
    for record in current.into_iter().chain(loaded) {
        if seen.insert(record.approval_id.clone()) {
            merged.push(record);
        }
    }
    merged.sort_by_key(|record| std::cmp::Reverse(record.updated_at));
    merged.truncate(MAX_MIGRATION_APPROVAL_RECORDS);
    merged
}

/// Returns a stable SHA-256 fingerprint without retaining the migration SQL itself.
fn migration_sql_digest(sql: &str) -> String {
    hex::encode(Sha256::digest(sql.as_bytes()))
}

/// Renders the latest bounded records so users can review outcomes without seeing SQL text.
pub(super) fn render_migration_approval_history(
    records: &[MigrationApprovalRecord],
    theme: &Theme,
) -> Option<AnyElement> {
    if records.is_empty() {
        return None;
    }
    let mut panel = v_flex().w_full().gap(px(4.0)).child(
        div()
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(format!("最近审批记录（{}）", records.len().min(5))),
    );
    for record in records.iter().take(5) {
        let (status, status_color) = match record.status {
            MigrationApprovalStatus::Approved => ("已确认", theme.warning),
            MigrationApprovalStatus::Executed => ("已执行", theme.success),
            MigrationApprovalStatus::Failed => ("执行失败", theme.danger),
        };
        let fingerprint = record.sql_sha256.chars().take(12).collect::<String>();
        let mut row = v_flex()
            .w_full()
            .gap(px(2.0))
            .p(px(8.0))
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().text_color(status_color).child(status))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{} · {}",
                                record.target_connection_name,
                                format_table_name(&record.target_schema, &record.target_table)
                            )),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(record.updated_at.format("%Y-%m-%d %H:%M:%S").to_string()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "脚本指纹 {} · {} 条语句 · {} · 驱动 {}",
                        fingerprint,
                        record.statement_count,
                        format_destructive_count(record.destructive_statements),
                        driver_label(record.driver)
                    )),
            );
        if record.warning_count > 0 {
            row = row.child(
                div()
                    .text_xs()
                    .text_color(theme.warning)
                    .child(format!("数据库警告 {} 条", record.warning_count)),
            );
        }
        if let Some(elapsed_ms) = record.elapsed_ms {
            row = row.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("数据库耗时 {elapsed_ms} ms")),
            );
        }
        if let Some(error) = &record.error {
            row = row.child(div().text_xs().text_color(theme.danger).child(format!(
                "{}：{}",
                "错误",
                bounded_text(error, 320)
            )));
        }
        panel = panel.child(row);
    }
    Some(panel.into_any_element())
}

fn format_table_name(schema: &str, table: &str) -> String {
    format!("{schema}.{table}")
}

fn format_destructive_count(count: usize) -> String {
    if count == 0 {
        "无删除或修改".into()
    } else {
        format!("{} 条删除或修改", count)
    }
}

fn driver_label(driver: DriverKind) -> &'static str {
    match driver {
        DriverKind::Mysql => "MySQL",
        DriverKind::Postgres => "PostgreSQL",
        DriverKind::Redis => "Redis",
        DriverKind::Mongodb => "MongoDB",
    }
}

fn validate_text(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > MAX_MIGRATION_APPROVAL_TEXT_BYTES {
        return Err(format!(
            "{label}为空或超过 {} bytes",
            MAX_MIGRATION_APPROVAL_TEXT_BYTES
        ));
    }
    Ok(())
}

fn bounded_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = 0;
    for (index, character) in value.char_indices() {
        if index + character.len_utf8() > max_bytes {
            break;
        }
        end = index + character.len_utf8();
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(id: &str) -> MigrationApprovalRecord {
        let now = Utc::now();
        MigrationApprovalRecord {
            approval_id: id.into(),
            source_connection_id: "source-id".into(),
            source_connection_name: "source".into(),
            target_connection_id: "target-id".into(),
            target_connection_name: "target".into(),
            source_schema: "public".into(),
            source_table: "source_table".into(),
            target_schema: "public".into(),
            target_table: "target_table".into(),
            driver: DriverKind::Postgres,
            statement_count: 2,
            destructive_statements: 1,
            sql_sha256: migration_sql_digest("ALTER TABLE target_table ADD COLUMN id integer;"),
            approved_at: now,
            updated_at: now,
            status: MigrationApprovalStatus::Approved,
            elapsed_ms: None,
            warning_count: 0,
            error: None,
        }
    }

    #[test]
    fn migration_digest_is_stable_without_exposing_sql() {
        let sql = "ALTER TABLE accounts ADD COLUMN active boolean;";
        let digest = migration_sql_digest(sql);
        assert_eq!(digest, migration_sql_digest(sql));
        assert_eq!(digest.len(), 64);
        assert_ne!(digest, sql);
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn parser_deduplicates_and_bounds_records() {
        let first = sample_record("first");
        let mut records = vec![first.clone(), first];
        records.extend(
            (0..=MAX_MIGRATION_APPROVAL_RECORDS)
                .map(|index| sample_record(&format!("record-{index}"))),
        );
        let json = serde_json::to_string(&records).expect("records should serialize");

        let (parsed, adjusted) = parse_migration_approvals(&json).expect("records should parse");
        assert!(adjusted);
        assert_eq!(parsed.len(), MAX_MIGRATION_APPROVAL_RECORDS);
        assert_eq!(
            parsed
                .iter()
                .filter(|record| record.approval_id == "first")
                .count(),
            1
        );
    }

    #[test]
    fn parser_rejects_oversized_preference() {
        assert!(
            parse_migration_approvals(&"x".repeat(MAX_MIGRATION_APPROVAL_PREF_BYTES + 1)).is_err()
        );
    }

    #[test]
    fn parser_rejects_invalid_script_fingerprint() {
        let mut record = sample_record("bad");
        record.sql_sha256 = "not-a-digest".into();
        let json = serde_json::to_string(&vec![record]).expect("record should serialize");
        let error = parse_migration_approvals(&json).expect_err("invalid digest should fail");
        assert!(error.contains("SHA-256"));
    }

    #[test]
    fn outcome_keeps_failure_details_bounded() {
        let mut record = sample_record("outcome");
        record.apply_outcome(
            MigrationApprovalStatus::Failed,
            None,
            0,
            Some(&"x".repeat(MAX_MIGRATION_APPROVAL_ERROR_BYTES + 1)),
        );
        assert_eq!(record.status, MigrationApprovalStatus::Failed);
        assert_eq!(record.elapsed_ms, None);
        assert_eq!(
            record.error.as_deref().map(str::len),
            Some(MAX_MIGRATION_APPROVAL_ERROR_BYTES)
        );
    }
}
