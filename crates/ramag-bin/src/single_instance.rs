//! Windows 单实例：命名互斥体判定首实例；后启进程经命名事件请求首实例唤起主窗口后退出。
//! `Local\` 前缀按登录会话隔离（多用户 / RDP 各自独立单实例）。
//! 守卫与等待线程退出时显式关闭内核句柄，激活事件经容量 1 通道合并重复请求。

use std::sync::mpsc::{Receiver, TrySendError, sync_channel};

use tracing::{info, warn};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
    WaitForSingleObject,
};
use windows::core::w;

pub(crate) enum InstanceRole {
    /// 本进程是唯一实例，持有互斥体并监听激活请求
    Primary(PrimaryGuard),
    /// 已有实例在运行：已请求其唤起主窗口，本进程应直接退出
    Secondary,
}

/// 首实例守卫：互斥体句柄随进程生命周期持有；rx 为 None 表示激活监听建立失败
/// （仅失去"双开时被唤起"能力，不影响其余功能）
pub(crate) struct PrimaryGuard {
    mutex: HANDLE,
    rx: Option<Receiver<()>>,
}

// HANDLE 是进程内有效的内核句柄值，跨线程传递安全；守卫仅在主线程使用
unsafe impl Send for PrimaryGuard {}

impl Drop for PrimaryGuard {
    fn drop(&mut self) {
        if !self.mutex.is_invalid() {
            close_handle(self.mutex, "close single-instance mutex failed");
        }
    }
}

impl PrimaryGuard {
    /// 非阻塞取激活请求（drain 后返回是否发生过）
    pub(crate) fn poll_activate(&self) -> bool {
        let Some(rx) = &self.rx else {
            return false;
        };
        let mut fired = false;
        while rx.try_recv().is_ok() {
            fired = true;
        }
        fired
    }
}

/// 进程启动最早期调用：判定首实例 / 通知已有实例
pub(crate) fn acquire() -> InstanceRole {
    let mutex = match unsafe { CreateMutexW(None, false, w!("Local\\RamagSingleInstanceMutex")) } {
        Ok(mutex) => mutex,
        Err(error) => {
            // 互斥体创建失败极罕见；宁可放行双开也不拒绝启动
            warn!(error = %error, "create single-instance mutex failed; continuing without it");
            return InstanceRole::Primary(PrimaryGuard {
                mutex: HANDLE::default(),
                rx: None,
            });
        }
    };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        close_handle(mutex, "close secondary-instance mutex failed");
        notify_primary();
        return InstanceRole::Secondary;
    }
    InstanceRole::Primary(PrimaryGuard {
        mutex,
        rx: spawn_activation_listener(),
    })
}

/// 创建激活事件（auto-reset）并起等待线程；触发经 channel 转出由主线程轮询
fn spawn_activation_listener() -> Option<Receiver<()>> {
    let event = match unsafe { CreateEventW(None, false, false, w!("Local\\RamagActivateEvent")) } {
        Ok(event) => event,
        Err(error) => {
            warn!(error = %error, "create activation event failed; reveal-on-relaunch disabled");
            return None;
        }
    };
    // 主线程只关心“至少发生过一次”，容量 1 可合并连续激活，避免异常事件洪泛积压内存。
    let (tx, rx) = sync_channel::<()>(1);
    let raw_event = event.0;
    let spawned = std::thread::Builder::new()
        .name("ramag-single-instance".into())
        .spawn(move || {
            let event = HANDLE(raw_event);
            loop {
                if unsafe { WaitForSingleObject(event, INFINITE) } != WAIT_OBJECT_0 {
                    warn!("wait activation event failed; listener stopped");
                    break;
                }
                match tx.try_send(()) {
                    Ok(()) | Err(TrySendError::Full(())) => {}
                    Err(TrySendError::Disconnected(())) => break,
                }
            }
            close_handle(event, "close activation event failed");
        });
    match spawned {
        Ok(_) => Some(rx),
        Err(error) => {
            warn!(error = %error, "start activation listener thread failed");
            close_handle(event, "close unused activation event failed");
            None
        }
    }
}

/// 后启进程：打开首实例的激活事件并置位
fn notify_primary() {
    match unsafe { OpenEventW(EVENT_MODIFY_STATE, false, w!("Local\\RamagActivateEvent")) } {
        Ok(event) => {
            if let Err(error) = unsafe { SetEvent(event) } {
                warn!(error = %error, "signal activation event failed");
            } else {
                info!("existing instance notified to reveal its window");
            }
            close_handle(event, "close activation notification event failed");
        }
        Err(error) => {
            warn!(error = %error, "open activation event failed; exiting without notify");
        }
    }
}

fn close_handle(handle: HANDLE, message: &'static str) {
    if let Err(error) = unsafe { CloseHandle(handle) } {
        warn!(error = %error, "{message}");
    }
}
