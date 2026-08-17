use serde::{Deserialize, Serialize};

pub const MAX_TRANSFER_WARNINGS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ConflictPolicy {
    #[default]
    Skip,
    /// 保留已存在对象，数据按条目去重补齐（SQL=INSERT IGNORE / ON CONFLICT DO NOTHING，
    /// Mongo=重复 `_id` 跳过；Redis 的 list/string 无法条目级去重，不支持该策略）
    Merge,
    Overwrite,
    Fail,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferProgress {
    pub stage: String,
    pub object: String,
    pub objects_done: u64,
    pub objects_total: Option<u64>,
    pub items_done: u64,
    pub bytes: u64,
}

impl TransferProgress {
    pub fn display_line(&self) -> String {
        let mut text = self.stage.replace(['\n', '\r'], " ");
        if !self.object.is_empty() {
            text.push(' ');
            text.push_str(&self.object.replace(['\n', '\r'], " "));
        }
        match self.objects_total {
            Some(total) if total > 0 => {
                text.push_str(&format!(" · {}/{total}", self.objects_done));
            }
            _ if self.objects_done > 0 => text.push_str(&format!(" · {}", self.objects_done)),
            _ => {}
        }
        if self.items_done > 0 {
            text.push_str(&format!(" · {} 条", self.items_done));
        }
        if self.bytes > 0 {
            text.push_str(&format!(" · {}", format_bytes(self.bytes)));
        }
        text
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let bytes_f = bytes as f64;
    if bytes_f >= KIB * KIB * KIB {
        format!("{:.2} GiB", bytes_f / (KIB * KIB * KIB))
    } else if bytes_f >= KIB * KIB {
        format!("{:.1} MiB", bytes_f / (KIB * KIB))
    } else if bytes_f >= KIB {
        format!("{:.0} KiB", bytes_f / KIB)
    } else {
        format!("{bytes} B")
    }
}

pub type ProgressFn<'a> = &'a (dyn Fn(TransferProgress) + Send + Sync);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferSummary {
    pub objects: u64,
    pub items: u64,
    pub skipped: u64,
    pub failed: u64,
    pub bytes: u64,
    pub elapsed_ms: u64,
    pub cancelled: bool,
    /// 警告明细（跳过原因 / 不支持对象），超上限只计数
    pub warnings: Vec<String>,
    pub warnings_overflow: u64,
}

impl TransferSummary {
    pub fn push_warning(&mut self, warning: impl Into<String>) {
        if self.warnings.len() < MAX_TRANSFER_WARNINGS {
            self.warnings.push(warning.into());
        } else {
            self.warnings_overflow += 1;
        }
    }

    pub fn brief(&self, verb: &str) -> String {
        let status = if self.cancelled {
            "已取消"
        } else {
            "完成"
        };
        let mut text = format!("{verb}{status}：{} 个对象、{} 条", self.objects, self.items);
        if self.skipped > 0 {
            text.push_str(&format!("，跳过 {}", self.skipped));
        }
        if self.failed > 0 {
            text.push_str(&format!("，失败 {}", self.failed));
        }
        let warning_count = self.warnings.len() as u64 + self.warnings_overflow;
        if warning_count > 0 {
            text.push_str(&format!("，警告 {warning_count}"));
        }
        text
    }

    pub fn merge(&mut self, other: TransferSummary) {
        self.objects = self.objects.saturating_add(other.objects);
        self.items = self.items.saturating_add(other.items);
        self.skipped = self.skipped.saturating_add(other.skipped);
        self.failed = self.failed.saturating_add(other.failed);
        self.elapsed_ms = self.elapsed_ms.saturating_add(other.elapsed_ms);
        self.cancelled |= other.cancelled;
        for warning in other.warnings {
            self.push_warning(warning);
        }
        self.warnings_overflow = self
            .warnings_overflow
            .saturating_add(other.warnings_overflow);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warnings_are_bounded() {
        let mut summary = TransferSummary::default();
        for i in 0..(MAX_TRANSFER_WARNINGS + 5) {
            summary.push_warning(format!("w{i}"));
        }
        assert_eq!(summary.warnings.len(), MAX_TRANSFER_WARNINGS);
        assert_eq!(summary.warnings_overflow, 5);
    }

    #[test]
    fn merge_accumulates_counts_and_cancel() {
        let mut total = TransferSummary {
            objects: 2,
            items: 10,
            skipped: 1,
            ..Default::default()
        };
        total.push_warning("a");
        total.merge(TransferSummary {
            objects: 3,
            items: 5,
            failed: 1,
            cancelled: true,
            warnings: vec!["b".into()],
            ..Default::default()
        });
        assert_eq!(total.objects, 5);
        assert_eq!(total.items, 15);
        assert_eq!(total.skipped, 1);
        assert_eq!(total.failed, 1);
        assert!(total.cancelled);
        assert_eq!(total.warnings, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn display_line_composes_progress_parts() {
        let progress = TransferProgress {
            stage: "导出数据".into(),
            object: "users".into(),
            objects_done: 3,
            objects_total: Some(17),
            items_done: 12_500,
            bytes: 4 * 1024 * 1024 + 200 * 1024,
        };
        let line = progress.display_line();
        assert!(line.starts_with("导出数据 users"));
        assert!(line.contains("3/17"));
        assert!(line.contains("12500 条"));
        assert!(line.contains("MiB"));
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2 KiB");
    }

    #[test]
    fn brief_mentions_skips_failures_and_cancel() {
        let mut summary = TransferSummary {
            objects: 3,
            items: 10,
            skipped: 1,
            failed: 2,
            cancelled: true,
            ..TransferSummary::default()
        };
        summary.push_warning("w");
        let text = summary.brief("导入");
        assert!(text.starts_with("导入已取消："));
        assert!(!text.contains("导入完成"));
        assert!(text.contains("3 个对象"));
        assert!(text.contains("跳过 1"));
        assert!(text.contains("失败 2"));
        assert!(text.contains("警告 1"));
    }
}
