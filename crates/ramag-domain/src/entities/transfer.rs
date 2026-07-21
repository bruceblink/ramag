//! 按库导出 / 导入共享实体：冲突策略、进度快照、结果汇总

use serde::{Deserialize, Serialize};

/// 汇总警告明细上限；超出后只累计数量，防止大库导出时无界膨胀
pub const MAX_TRANSFER_WARNINGS: usize = 100;

/// 导入冲突处理策略（同名表 / collection / key 已存在时）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ConflictPolicy {
    /// 跳过已存在对象，其余继续（默认）
    #[default]
    Skip,
    /// 保留已存在对象，数据按条目去重补齐（SQL=INSERT IGNORE / ON CONFLICT DO NOTHING，
    /// Mongo=重复 `_id` 跳过；Redis 的 list/string 无法条目级去重，不支持该策略）
    Merge,
    /// 先删除已存在对象再导入
    Overwrite,
    /// 遇到冲突立即终止
    Fail,
}

/// 导出 / 导入进度快照，经回调推给 UI；UI 侧自行节流渲染
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferProgress {
    /// 当前阶段（如「导出表结构」「写入数据」）
    pub stage: String,
    /// 当前对象（表 / collection / key）
    pub object: String,
    pub objects_done: u64,
    /// None = 总数未知
    pub objects_total: Option<u64>,
    /// 已处理条目（行 / 文档 / key）
    pub items_done: u64,
    pub bytes: u64,
}

impl TransferProgress {
    /// 工具栏单行进度文案，如「导出数据 users · 3/17 · 12500 条 · 4.2 MiB」。
    /// 压成单行（GPUI 单行 shaping 不允许换行符）
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

/// 人读字节数（KiB / MiB / GiB 两位精度）
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

/// 进度回调引用。服务层在批次边界调用
pub type ProgressFn<'a> = &'a (dyn Fn(TransferProgress) + Send + Sync);

/// 导出 / 导入结果汇总。取消不是错误：`cancelled=true` + 已完成部分留在计数里
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferSummary {
    /// 完成对象数（表 / collection / key）
    pub objects: u64,
    /// 条目数（行 / 文档 / 集合元素）
    pub items: u64,
    /// 冲突策略跳过的对象数
    pub skipped: u64,
    /// 失败对象数（导入逐对象容错时累计）
    pub failed: u64,
    pub bytes: u64,
    pub elapsed_ms: u64,
    /// 用户中途取消
    pub cancelled: bool,
    /// 警告明细（跳过原因 / 不支持对象），超上限只计数
    pub warnings: Vec<String>,
    /// 未纳入明细的额外警告数
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

    /// 状态栏一句话摘要
    pub fn brief(&self, verb: &str) -> String {
        let mut text = format!("{verb}完成：{} 个对象、{} 条", self.objects, self.items);
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
        if self.cancelled {
            text.push_str("（已取消）");
        }
        text
    }

    /// 多文件导入逐文件累加计数；警告受同一上限约束，任一文件取消即整体取消。
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
        assert!(text.contains("3 个对象"));
        assert!(text.contains("跳过 1"));
        assert!(text.contains("失败 2"));
        assert!(text.contains("警告 1"));
        assert!(text.contains("已取消"));
    }
}
