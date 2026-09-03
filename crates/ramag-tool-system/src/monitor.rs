//! 本机系统指标采集与进程安全操作。

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sysinfo::{Disks, Networks, Pid, System};

/// 图表保留的滚动时间窗口，和 AppWorkbench 的原始行为保持一致。
pub const HISTORY_SECONDS: f64 = 60.0;
/// 进程页最多渲染的行数，避免系统进程数量异常时创建无界 UI 元素。
pub const MAX_VISIBLE_PROCESSES: usize = 120;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProcessSort {
    #[default]
    Cpu,
    Memory,
}

impl ProcessSort {
    /// 返回排序按钮使用的短标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RefreshInterval {
    #[default]
    OneSecond,
    TwoSeconds,
    FiveSeconds,
}

impl RefreshInterval {
    /// 把用户可选的刷新档位转换为后台轮询周期。
    pub fn duration(self) -> Duration {
        Duration::from_secs(match self {
            Self::OneSecond => 1,
            Self::TwoSeconds => 2,
            Self::FiveSeconds => 5,
        })
    }

    /// 返回设置栏显示的紧凑标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::OneSecond => "1s",
            Self::TwoSeconds => "2s",
            Self::FiveSeconds => "5s",
        }
    }

    /// 返回状态提示使用的完整英文单位，便于和数值区分。
    pub fn status_label(self) -> &'static str {
        match self {
            Self::OneSecond => "1 second",
            Self::TwoSeconds => "2 seconds",
            Self::FiveSeconds => "5 seconds",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskSnapshot {
    pub device: String,
    pub mount_point: String,
    pub file_system: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub usage_percent: u8,
}

#[derive(Clone, Debug, Default)]
pub struct MonitorSnapshot {
    pub elapsed_seconds: f64,
    pub cpu_percent: f32,
    pub core_usages: Vec<f32>,
    pub core_histories: Vec<Vec<[f64; 2]>>,
    pub memory_total: u64,
    pub memory_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub disk_read_rate_mb: f64,
    pub disk_write_rate_mb: f64,
    pub network_received_rate_mb: f64,
    pub network_transmitted_rate_mb: f64,
    pub processes: Vec<ProcessSnapshot>,
    pub disks: Vec<DiskSnapshot>,
    pub cpu_history: Vec<[f64; 2]>,
    pub memory_history: Vec<[f64; 2]>,
    pub swap_history: Vec<[f64; 2]>,
    pub disk_read_history: Vec<[f64; 2]>,
    pub disk_write_history: Vec<[f64; 2]>,
    pub network_received_history: Vec<[f64; 2]>,
    pub network_transmitted_history: Vec<[f64; 2]>,
    pub data_warning: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminateResult {
    RefusedSelf {
        pid: u32,
    },
    Missing {
        pid: u32,
    },
    Changed {
        pid: u32,
        expected_name: String,
        actual_name: String,
    },
    Sent {
        pid: u32,
        name: String,
    },
    Failed {
        pid: u32,
        name: String,
    },
}

/// 可在线程间共享的系统采集器；锁只保护采集器状态，不把 GPUI 类型带入采集层。
#[derive(Clone)]
pub struct SystemMonitor {
    state: Arc<Mutex<MonitorState>>,
}

impl Default for SystemMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemMonitor {
    /// 创建空采集器；昂贵的系统读取会在后台首次刷新时执行。
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(MonitorState::new())),
        }
    }

    /// 读取一份独立快照，避免渲染期间持有锁或暴露内部可变状态。
    pub fn snapshot(&self) -> MonitorSnapshot {
        self.state.lock().snapshot.clone()
    }

    #[cfg(test)]
    pub(crate) fn set_snapshot_for_test(&self, snapshot: MonitorSnapshot) {
        self.state.lock().snapshot = snapshot;
    }

    /// 读取当前刷新档位，不暴露内部锁或采集器对象。
    pub fn refresh_interval(&self) -> RefreshInterval {
        self.state.lock().refresh_interval
    }

    /// 更新刷新档位，下一次后台轮询按新周期判断是否采样。
    pub fn set_refresh_interval(&self, interval: RefreshInterval) {
        self.state.lock().refresh_interval = interval;
    }

    /// 读取当前进程排序规则，供视图绘制选中状态。
    pub fn process_sort(&self) -> ProcessSort {
        self.state.lock().process_sort
    }

    /// 更新排序规则并立即重排已缓存的进程快照。
    pub fn set_process_sort(&self, sort: ProcessSort) {
        let mut state = self.state.lock();
        state.process_sort = sort;
        sort_processes(&mut state.snapshot.processes, sort);
    }

    /// 返回距上次成功采样的时间，用于诊断后台刷新是否停滞。
    pub fn last_refresh_age(&self) -> Option<Duration> {
        self.state.lock().last_refresh_at.map(|last| last.elapsed())
    }

    /// 后台轮询调用此方法；到达刷新周期时采集一次并返回 true。
    pub fn refresh_if_due(&self) -> bool {
        let mut state = self.state.lock();
        if !state.refresh_due() {
            return false;
        }
        state.refresh_now();
        true
    }

    /// 手动刷新使用此入口，调用方应把它放到 GPUI 后台任务中执行。
    pub fn refresh_now(&self) {
        self.state.lock().refresh_now();
    }

    /// 终止前重新读取进程并核对名称，降低 PID 被复用时误杀其它进程的风险。
    pub fn terminate_process(&self, pid: u32, expected_name: &str) -> TerminateResult {
        let mut state = self.state.lock();
        if pid == std::process::id() {
            return TerminateResult::RefusedSelf { pid };
        }

        state.system.refresh_all();
        let Some(process) = state.system.process(Pid::from_u32(pid)) else {
            return TerminateResult::Missing { pid };
        };
        let actual_name = process.name().to_string_lossy().into_owned();
        if actual_name != expected_name {
            return TerminateResult::Changed {
                pid,
                expected_name: expected_name.to_owned(),
                actual_name,
            };
        }
        if process.kill() {
            TerminateResult::Sent {
                pid,
                name: actual_name,
            }
        } else {
            TerminateResult::Failed {
                pid,
                name: actual_name,
            }
        }
    }
}

struct MonitorState {
    system: System,
    networks: Networks,
    disks: Disks,
    started_at: Instant,
    last_refresh_at: Option<Instant>,
    previous_disk_read_total: Option<u64>,
    previous_disk_write_total: Option<u64>,
    previous_network_received_total: Option<u64>,
    previous_network_transmitted_total: Option<u64>,
    refresh_interval: RefreshInterval,
    process_sort: ProcessSort,
    snapshot: MonitorSnapshot,
}

impl MonitorState {
    fn new() -> Self {
        Self {
            // 只创建采集器，首份数据由后台任务刷新，避免打开工具时阻塞 GPUI 线程。
            system: System::new(),
            networks: Networks::new(),
            disks: Disks::new(),
            started_at: Instant::now(),
            last_refresh_at: None,
            previous_disk_read_total: None,
            previous_disk_write_total: None,
            previous_network_received_total: None,
            previous_network_transmitted_total: None,
            refresh_interval: RefreshInterval::default(),
            process_sort: ProcessSort::default(),
            snapshot: MonitorSnapshot::default(),
        }
    }

    fn refresh_due(&self) -> bool {
        self.last_refresh_at
            .is_none_or(|last| last.elapsed() >= self.refresh_interval.duration())
    }

    /// 采集 CPU、内存、进程、磁盘和网络，并把历史数据截断到 60 秒。
    fn refresh_now(&mut self) {
        let now = Instant::now();
        let elapsed_seconds = now.duration_since(self.started_at).as_secs_f64();
        let interval_seconds = self
            .last_refresh_at
            .map(|last| now.duration_since(last).as_secs_f64())
            .unwrap_or_else(|| self.refresh_interval.duration().as_secs_f64())
            .max(f64::EPSILON);

        self.system.refresh_all();
        let mut processes = Vec::with_capacity(self.system.processes().len());
        let mut disk_read_total = 0_u64;
        let mut disk_write_total = 0_u64;
        for (pid, process) in self.system.processes() {
            let disk_usage = process.disk_usage();
            disk_read_total = disk_read_total.saturating_add(disk_usage.total_read_bytes);
            disk_write_total = disk_write_total.saturating_add(disk_usage.total_written_bytes);
            processes.push(ProcessSnapshot {
                pid: pid.as_u32(),
                name: process.name().to_string_lossy().into_owned(),
                cpu_percent: process.cpu_usage(),
                memory_bytes: process.memory(),
            });
        }
        sort_processes(&mut processes, self.process_sort);

        let cpu_percent = self.system.global_cpu_usage();
        let core_usages = self
            .system
            .cpus()
            .iter()
            .map(|cpu| cpu.cpu_usage())
            .collect::<Vec<_>>();
        let mut core_histories = self.snapshot.core_histories.clone();
        update_core_histories(
            &mut core_histories,
            &core_usages,
            elapsed_seconds,
            elapsed_seconds - HISTORY_SECONDS,
        );

        self.networks.refresh(true);
        let (network_received_total, network_transmitted_total) = self.network_totals();
        self.disks.refresh(true);
        let disks = collect_disks(&self.disks);

        let disk_read_rate_mb = counter_rate(
            disk_read_total,
            self.previous_disk_read_total,
            interval_seconds,
        );
        let disk_write_rate_mb = counter_rate(
            disk_write_total,
            self.previous_disk_write_total,
            interval_seconds,
        );
        let network_received_rate_mb = counter_rate(
            network_received_total,
            self.previous_network_received_total,
            interval_seconds,
        );
        let network_transmitted_rate_mb = counter_rate(
            network_transmitted_total,
            self.previous_network_transmitted_total,
            interval_seconds,
        );

        let memory_total = self.system.total_memory();
        let memory_used = self.system.used_memory();
        let swap_total = self.system.total_swap();
        let swap_used = self.system.used_swap();
        let mut snapshot = MonitorSnapshot {
            elapsed_seconds,
            cpu_percent,
            core_usages,
            core_histories,
            memory_total,
            memory_used,
            swap_total,
            swap_used,
            disk_read_rate_mb,
            disk_write_rate_mb,
            network_received_rate_mb,
            network_transmitted_rate_mb,
            processes,
            disks,
            ..self.snapshot.clone()
        };

        push_history(
            &mut snapshot.cpu_history,
            elapsed_seconds,
            cpu_percent as f64,
        );
        push_history(
            &mut snapshot.memory_history,
            elapsed_seconds,
            percentage(memory_used, memory_total),
        );
        push_history(
            &mut snapshot.swap_history,
            elapsed_seconds,
            percentage(swap_used, swap_total),
        );
        push_history(
            &mut snapshot.disk_read_history,
            elapsed_seconds,
            disk_read_rate_mb,
        );
        push_history(
            &mut snapshot.disk_write_history,
            elapsed_seconds,
            disk_write_rate_mb,
        );
        push_history(
            &mut snapshot.network_received_history,
            elapsed_seconds,
            network_received_rate_mb,
        );
        push_history(
            &mut snapshot.network_transmitted_history,
            elapsed_seconds,
            network_transmitted_rate_mb,
        );

        let minimum_elapsed = elapsed_seconds - HISTORY_SECONDS;
        trim_history(&mut snapshot.cpu_history, minimum_elapsed);
        for history in &mut snapshot.core_histories {
            trim_history(history, minimum_elapsed);
        }
        trim_history(&mut snapshot.memory_history, minimum_elapsed);
        trim_history(&mut snapshot.swap_history, minimum_elapsed);
        trim_history(&mut snapshot.disk_read_history, minimum_elapsed);
        trim_history(&mut snapshot.disk_write_history, minimum_elapsed);
        trim_history(&mut snapshot.network_received_history, minimum_elapsed);
        trim_history(&mut snapshot.network_transmitted_history, minimum_elapsed);

        let mut warnings = Vec::new();
        if snapshot.core_usages.is_empty() {
            warnings.push("CPU 核心数据不可用");
        }
        if snapshot.processes.is_empty() {
            warnings.push("进程列表不可用");
        }
        if snapshot.disks.is_empty() {
            warnings.push("磁盘列表不可用");
        }
        snapshot.data_warning = (!warnings.is_empty()).then(|| warnings.join("; "));

        self.snapshot = snapshot;
        self.previous_disk_read_total = Some(disk_read_total);
        self.previous_disk_write_total = Some(disk_write_total);
        self.previous_network_received_total = Some(network_received_total);
        self.previous_network_transmitted_total = Some(network_transmitted_total);
        self.last_refresh_at = Some(now);
    }

    fn network_totals(&self) -> (u64, u64) {
        self.networks
            .iter()
            .fold((0_u64, 0_u64), |(received, transmitted), (_, data)| {
                (
                    received.saturating_add(data.total_received()),
                    transmitted.saturating_add(data.total_transmitted()),
                )
            })
    }
}

fn sort_processes(processes: &mut [ProcessSnapshot], sort: ProcessSort) {
    processes.sort_by(|left, right| {
        let ordering = match sort {
            ProcessSort::Cpu => right.cpu_percent.total_cmp(&left.cpu_percent),
            ProcessSort::Memory => right.memory_bytes.cmp(&left.memory_bytes),
        };
        ordering.then_with(|| left.name.cmp(&right.name))
    });
}

fn collect_disks(disks: &Disks) -> Vec<DiskSnapshot> {
    disks
        .iter()
        .map(|disk| {
            let total_bytes = disk.total_space();
            let available_bytes = disk.available_space();
            let used_bytes = total_bytes.saturating_sub(available_bytes);
            let mount_point = disk.mount_point().to_string_lossy().into_owned();
            let name = disk.name().to_string_lossy().into_owned();
            DiskSnapshot {
                device: if name.trim().is_empty() {
                    mount_point.clone()
                } else {
                    name
                },
                mount_point,
                file_system: disk.file_system().to_string_lossy().into_owned(),
                total_bytes,
                used_bytes,
                available_bytes,
                usage_percent: percentage(used_bytes, total_bytes).round() as u8,
            }
        })
        .collect()
}

/// 用两次累计计数的差值计算每秒 MiB；首次采样只建立基线。
fn counter_rate(current: u64, previous: Option<u64>, interval_seconds: f64) -> f64 {
    let Some(previous) = previous else {
        return 0.0;
    };
    current.saturating_sub(previous) as f64 / 1_000_000.0 / interval_seconds.max(f64::EPSILON)
}

/// 计算占用百分比并处理零容量和异常超界值。
fn percentage(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        ((used as f64 / total as f64) * 100.0).clamp(0.0, 100.0)
    }
}

fn push_history(history: &mut Vec<[f64; 2]>, elapsed: f64, value: f64) {
    history.push([elapsed, value]);
}

/// 按当前 CPU 核心数调整历史列表，并追加本次采样，防止核心数量变化后索引错位。
fn update_core_histories(
    histories: &mut Vec<Vec<[f64; 2]>>,
    usages: &[f32],
    elapsed: f64,
    minimum_elapsed: f64,
) {
    histories.truncate(usages.len());
    histories.resize_with(usages.len(), Vec::new);
    for (history, usage) in histories.iter_mut().zip(usages) {
        push_history(history, elapsed, f64::from(*usage));
        trim_history(history, minimum_elapsed);
    }
}

/// 删除 60 秒窗口之前的点，限制历史数据的内存增长。
fn trim_history(history: &mut Vec<[f64; 2]>, minimum_elapsed: f64) {
    history.retain(|point| point[0] >= minimum_elapsed);
}

#[cfg(test)]
mod tests {
    use super::{
        ProcessSnapshot, ProcessSort, SystemMonitor, TerminateResult, counter_rate, percentage,
        sort_processes, trim_history, update_core_histories,
    };

    #[test]
    fn percentage_handles_zero_and_caps() {
        assert_eq!(percentage(1, 0), 0.0);
        assert_eq!(percentage(50, 100), 50.0);
        assert_eq!(percentage(120, 100), 100.0);
    }

    #[test]
    fn counter_rate_uses_saturating_delta() {
        assert_eq!(counter_rate(3_000_000, Some(1_000_000), 2.0), 1.0);
        assert_eq!(counter_rate(1_000_000, Some(3_000_000), 2.0), 0.0);
        assert_eq!(counter_rate(1_000_000, None, 2.0), 0.0);
    }

    #[test]
    fn history_removes_points_before_threshold() {
        let mut history = vec![[1.0, 10.0], [40.0, 20.0], [61.0, 30.0]];
        trim_history(&mut history, 41.0);
        assert_eq!(history, vec![[61.0, 30.0]]);
    }

    #[test]
    fn core_histories_follow_the_current_core_count() {
        let mut histories = vec![vec![[1.0, 10.0]], vec![[61.0, 20.0]], vec![[61.0, 30.0]]];
        update_core_histories(&mut histories, &[42.0, 8.0], 62.0, 2.0);

        assert_eq!(histories.len(), 2);
        assert_eq!(histories[0].last(), Some(&[62.0, 42.0]));
        assert_eq!(histories[1].last(), Some(&[62.0, 8.0]));
    }

    #[test]
    fn process_sort_orders_cpu_and_memory_descending() {
        let mut rows = vec![
            ProcessSnapshot {
                pid: 1,
                name: "alpha".to_owned(),
                cpu_percent: 2.0,
                memory_bytes: 30,
            },
            ProcessSnapshot {
                pid: 2,
                name: "beta".to_owned(),
                cpu_percent: 4.0,
                memory_bytes: 10,
            },
        ];
        sort_processes(&mut rows, ProcessSort::Cpu);
        assert_eq!(rows[0].pid, 2);
        sort_processes(&mut rows, ProcessSort::Memory);
        assert_eq!(rows[0].pid, 1);
    }

    #[test]
    fn terminate_refuses_the_current_process() {
        let monitor = SystemMonitor::new();
        let result = monitor.terminate_process(std::process::id(), "ramag");
        assert_eq!(
            result,
            TerminateResult::RefusedSelf {
                pid: std::process::id()
            }
        );
    }
}
