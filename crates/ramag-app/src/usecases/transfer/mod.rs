//! SQL、MongoDB 与 Redis 的流式导入导出编排。
//!
//! 导出：async 生产者分批读库 → 有界通道 → 阻塞池 `write_atomic_with` 落盘；
//! 只有生产者显式 `finish()` 才提交文件，取消或出错都不会留下半截文件。
//! 导入：流式读文件、分批写库。进度按批次回调，`AtomicBool` 随时取消；
//! 取消时返回 `cancelled=true`，并保留已导入的数据。

pub mod jsonl_table;
pub mod mongo;
pub mod redis;
mod redis_selection_export;
mod redis_selection_import;
pub(crate) mod sql_catalog;
pub mod sql_export;
pub mod sql_import;

pub use jsonl_table::import_jsonl_into_table;
pub use mongo::{
    export_mongo_collection, export_mongo_database, import_jsonl_into_collection,
    import_mongo_collection, import_mongo_database,
};
pub use redis::{export_redis_db, import_redis_db};
pub use redis_selection_export::{export_redis_key, export_redis_prefix};
pub use redis_selection_import::import_redis_selection;
pub use sql_export::{export_sql_database, export_sql_table};
pub use sql_import::{import_sql_database, import_sql_table};

use std::io::BufRead as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::time::Instant;

use ramag_domain::entities::{ProgressFn, TransferProgress, TransferSummary};
use ramag_domain::error::{DomainError, Result};

/// 导出块达到该大小后再投递，减少通道往返。
const SINK_FLUSH_BYTES: usize = 256 * 1024;
const SINK_QUEUE_BLOCKS: usize = 64;
/// 生产端提前退出时通知写线程放弃临时文件。
const SINK_ABORT_MESSAGE: &str = "__ramag_transfer_abort__";
const SINK_CLOSED_MESSAGE: &str = "导出写文件线程已退出";
/// 为 MySQL 外键检查前缀预留批次空间。
pub(crate) const MYSQL_IMPORT_PREFIX: &str = "SET FOREIGN_KEY_CHECKS=0;\n";

enum SinkMsg {
    Chunk(Vec<u8>),
    Finish,
}

/// 带缓冲的导出文件写入端。
pub(crate) struct ExportSink {
    sender: SyncSender<SinkMsg>,
    buffer: Vec<u8>,
    bytes: u64,
}

impl ExportSink {
    pub(crate) fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.buffer.extend_from_slice(bytes);
        self.bytes += bytes.len() as u64;
        if self.buffer.len() >= SINK_FLUSH_BYTES {
            self.flush()?;
        }
        Ok(())
    }

    pub(crate) fn write_str(&mut self, text: &str) -> Result<()> {
        self.write(text.as_bytes())
    }

    pub(crate) fn bytes_written(&self) -> u64 {
        self.bytes
    }

    fn flush(&mut self) -> Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::take(&mut self.buffer);
        self.buffer.reserve(SINK_FLUSH_BYTES);
        self.sender
            .send(SinkMsg::Chunk(chunk))
            .map_err(|_| DomainError::Storage(SINK_CLOSED_MESSAGE.into()))
    }

    /// 提交文件；未调用时清理临时文件并保留原文件。
    pub(crate) fn finish(mut self) -> Result<()> {
        self.flush()?;
        self.sender
            .send(SinkMsg::Finish)
            .map_err(|_| DomainError::Storage(SINK_CLOSED_MESSAGE.into()))
    }
}

enum WriteOutcome {
    Committed,
    Aborted,
}

/// 组装阻塞写线程与异步生产者；仅调用 `finish()` 后提交文件。
pub(crate) async fn with_export_sink<T, F, Fut>(path: &Path, produce: F) -> Result<T>
where
    F: FnOnce(ExportSink) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let (sender, receiver) = std::sync::mpsc::sync_channel::<SinkMsg>(SINK_QUEUE_BLOCKS);
    let target = path.to_path_buf();
    let writer = crate::run_blocking(move || {
        let write_result = super::export::write_atomic_with(&target, |writer| {
            loop {
                match receiver.recv() {
                    Ok(SinkMsg::Chunk(chunk)) => writer.write_all(&chunk).map_err(|error| {
                        DomainError::Storage(format!("写入导出文件失败：{error}"))
                    })?,
                    Ok(SinkMsg::Finish) => return Ok(()),
                    Err(_) => return Err(DomainError::Storage(SINK_ABORT_MESSAGE.into())),
                }
            }
        });
        match write_result {
            Ok(()) => Ok(WriteOutcome::Committed),
            Err(error) if error.message() == SINK_ABORT_MESSAGE => Ok(WriteOutcome::Aborted),
            Err(error) => Err(error),
        }
    });
    let sink = ExportSink {
        sender,
        buffer: Vec::with_capacity(SINK_FLUSH_BYTES),
        bytes: 0,
    };
    let (write_result, produce_result) = futures::join!(writer, produce(sink));
    match (write_result, produce_result) {
        (Ok(_), Ok(value)) => Ok(value),
        // 写盘失败会导致生产端发送失败，应优先返回根因。
        (Err(write_error), Err(produce_error)) => {
            if produce_error.message() == SINK_CLOSED_MESSAGE {
                Err(write_error)
            } else {
                Err(produce_error)
            }
        }
        (Err(write_error), Ok(_)) => Err(write_error),
        (Ok(_), Err(produce_error)) => Err(produce_error),
    }
}

pub(crate) fn is_cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

/// 有界读取一行，避免超长文件先触发大额分配；返回 0 表示 EOF。
pub(crate) fn read_line_bounded(
    reader: &mut impl std::io::BufRead,
    line: &mut String,
    max_bytes: usize,
    label: &str,
) -> Result<usize> {
    line.clear();
    let read_limit = max_bytes.saturating_add(1);
    let mut limited =
        std::io::Read::take(&mut *reader, u64::try_from(read_limit).unwrap_or(u64::MAX));
    let read = limited
        .read_line(line)
        .map_err(|error| DomainError::Storage(format!("读取{label}失败：{error}")))?;
    if line.len() > max_bytes {
        return Err(DomainError::InvalidConfig(format!(
            "{label}单行超过 {} MiB 安全上限，疑似损坏",
            max_bytes / 1024 / 1024
        )));
    }
    Ok(read)
}

/// 使用复用缓冲区写入一条 JSONL 记录。
pub(crate) fn write_json_line(
    sink: &mut ExportSink,
    buffer: &mut Vec<u8>,
    value: &serde_json::Value,
) -> Result<()> {
    buffer.clear();
    serde_json::to_writer(&mut *buffer, value)
        .map_err(|error| DomainError::Storage(format!("序列化导出记录失败：{error}")))?;
    buffer.push(b'\n');
    if buffer.len() > ramag_domain::entities::TRANSFER_BATCH_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "单条导出记录超过 {} MiB 安全上限，无法继续导出",
            ramag_domain::entities::TRANSFER_BATCH_BYTES / 1024 / 1024
        )));
    }
    sink.write(buffer)
}

/// 保存进度快照，并支持高频场景节流上报。
pub(crate) struct Reporter<'a> {
    emit: ProgressFn<'a>,
    pub snapshot: TransferProgress,
    ticks: u32,
}

impl<'a> Reporter<'a> {
    pub(crate) fn new(emit: ProgressFn<'a>) -> Self {
        Self {
            emit,
            snapshot: TransferProgress::default(),
            ticks: 0,
        }
    }

    pub(crate) fn stage(&mut self, stage: impl Into<String>, object: impl Into<String>) {
        self.snapshot.stage = stage.into();
        self.snapshot.object = object.into();
        self.emit();
    }

    pub(crate) fn emit(&self) {
        (self.emit)(self.snapshot.clone());
    }

    /// 每调用 `every` 次上报一次。
    pub(crate) fn emit_every(&mut self, every: u32) {
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks.is_multiple_of(every.max(1)) {
            self.emit();
        }
    }
}

/// 写入耗时并返回汇总。
pub(crate) fn finish_summary(mut summary: TransferSummary, start: Instant) -> TransferSummary {
    summary.elapsed_ms = start.elapsed().as_millis() as u64;
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_progress() -> impl Fn(TransferProgress) + Send + Sync {
        |_| {}
    }

    #[test]
    fn sink_commits_only_after_finish() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("ramag-transfer-sink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| DomainError::Storage(e.to_string()))?;
        let committed = dir.join("committed.txt");
        let aborted = dir.join("aborted.txt");

        futures::executor::block_on(async {
            let value = with_export_sink(&committed, |mut sink| async move {
                sink.write_str("hello ")?;
                sink.write_str("world")?;
                assert_eq!(sink.bytes_written(), 11);
                sink.finish()?;
                Ok(42)
            })
            .await?;
            assert_eq!(value, 42);

            let cancelled = with_export_sink(&aborted, |mut sink| async move {
                sink.write_str("partial")?;
                Ok(7)
            })
            .await?;
            assert_eq!(cancelled, 7);
            Ok::<(), DomainError>(())
        })?;

        assert_eq!(
            std::fs::read_to_string(&committed).map_err(|e| DomainError::Storage(e.to_string()))?,
            "hello world"
        );
        assert!(!aborted.exists());
        std::fs::remove_dir_all(&dir).map_err(|e| DomainError::Storage(e.to_string()))?;
        Ok(())
    }

    #[test]
    fn reporter_throttles_high_frequency_emits() {
        let count = std::sync::atomic::AtomicU32::new(0);
        let emit = |_p: TransferProgress| {
            count.fetch_add(1, Ordering::Relaxed);
        };
        let mut reporter = Reporter::new(&emit);
        for _ in 0..64 {
            reporter.emit_every(16);
        }
        assert_eq!(count.load(Ordering::Relaxed), 4);
        let _ = noop_progress();
    }

    #[test]
    fn bounded_line_reader_accepts_exact_boundary_and_rejects_before_full_line_load() {
        let mut exact = std::io::Cursor::new(b"abcd".as_slice());
        let mut line = String::new();
        assert_eq!(
            read_line_bounded(&mut exact, &mut line, 4, "测试").unwrap(),
            4
        );
        assert_eq!(line, "abcd");

        let mut over = std::io::Cursor::new(b"abcd\nremaining".as_slice());
        let error = read_line_bounded(&mut over, &mut line, 4, "测试").unwrap_err();
        assert!(error.message().contains("单行超过"));
        assert!(line.len() <= 5);
    }
}
