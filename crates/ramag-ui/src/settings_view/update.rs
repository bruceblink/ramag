//! 关于与更新页面。

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, Styled, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, button::ButtonVariants as _, h_flex,
    notification::Notification, v_flex,
};
use ramag_app::{AvailableUpdate, UpdateCheckResult};
use ramag_domain::entities::{DownloadProgress, UpdateCancellation, UpdateProgressFn};

use super::{SettingsView, UpdateUiState, pages::settings_card};

impl SettingsView {
    pub(super) fn sync_update_state(&mut self) {
        if !matches!(self.update_state, UpdateUiState::Idle) {
            return;
        }
        let Some(result) = self
            .update_service
            .as_ref()
            .and_then(|service| service.last_result())
        else {
            return;
        };
        self.apply_update_result(result);
    }

    fn apply_update_result(&mut self, result: UpdateCheckResult) {
        let previous_version =
            update_from_state(&self.update_state).map(|update| update.release.version.as_str());
        let next_version = match &result {
            UpdateCheckResult::Available(update)
            | UpdateCheckResult::UnsupportedPlatform(update) => {
                Some(update.release.version.as_str())
            }
            _ => None,
        };
        if previous_version != next_version {
            self.update_downloaded_path = None;
            self.update_download_error = None;
        }
        self.update_state = match result {
            UpdateCheckResult::Skipped => return,
            UpdateCheckResult::UpToDate { latest_version, .. } => {
                UpdateUiState::UpToDate { latest_version }
            }
            UpdateCheckResult::Available(update) => UpdateUiState::Available(update),
            UpdateCheckResult::UnsupportedPlatform(update) => {
                UpdateUiState::UnsupportedPlatform(update)
            }
        };
    }

    fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        if self.update_downloading || matches!(self.update_state, UpdateUiState::Checking) {
            return;
        }
        let Some(service) = self.update_service.clone() else {
            self.update_state = UpdateUiState::Error("更新检查组件初始化失败".into());
            cx.notify();
            return;
        };
        self.update_state = UpdateUiState::Checking;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = service.check(true).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(result) => {
                        crate::sync_update_indicator(&result, cx);
                        this.apply_update_result(result);
                    }
                    Err(error) => {
                        this.update_state = UpdateUiState::Error(error.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_update_download(&mut self, cx: &mut Context<Self>) {
        if self.update_downloading {
            return;
        }
        let Some(service) = self.update_service.clone() else {
            return;
        };
        let Some(update) = update_from_state(&self.update_state).cloned() else {
            return;
        };
        if update.asset.is_none() {
            return;
        }

        self.update_downloading = true;
        self.update_download_error = None;
        self.update_downloaded_path = None;
        let cancellation = UpdateCancellation::default();
        self.update_cancellation = Some(cancellation.clone());
        let progress = self.update_progress.clone();
        *progress.lock() = DownloadProgress {
            downloaded: 0,
            total: update.asset.as_ref().map_or(0, |asset| asset.size),
        };
        cx.notify();

        let progress_for_download = progress.clone();
        let progress_fn: UpdateProgressFn = Arc::new(move |value| {
            *progress_for_download.lock() = value;
        });
        let cancellation_for_result = cancellation.clone();
        cx.spawn(async move |this, cx| {
            let result = service
                .download(&update, progress_fn, cancellation_for_result.clone())
                .await;
            let _ = this.update(cx, |this, cx| {
                this.update_downloading = false;
                this.update_cancellation = None;
                match result {
                    Ok(path) => {
                        this.update_downloaded_path = Some(path);
                        this.pending_notification =
                            Some(Notification::success("安装包下载并校验完成").autohide(true));
                    }
                    Err(_) if cancellation_for_result.is_cancelled() => {
                        this.pending_notification =
                            Some(Notification::info("已取消更新下载").autohide(true));
                    }
                    Err(error) => {
                        this.update_download_error = Some(error.to_string());
                        this.pending_notification = Some(
                            Notification::error("更新下载失败，请查看页面详情").autohide(true),
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                let keep_polling = this
                    .update(cx, |this, cx| {
                        if this.update_downloading {
                            cx.notify();
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !keep_polling {
                    break;
                }
            }
        })
        .detach();
    }

    fn cancel_update_download(&mut self, cx: &mut Context<Self>) {
        if let Some(cancellation) = &self.update_cancellation {
            cancellation.cancel();
            cx.notify();
        }
    }

    fn reveal_download(&mut self, cx: &mut Context<Self>) {
        let (Some(service), Some(path)) = (
            self.update_service.clone(),
            self.update_downloaded_path.clone(),
        ) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = ramag_app::run_blocking(move || service.reveal_download(&path)).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.pending_notification = Some(
                        Notification::error(format!("打开安装包目录失败：{error}")).autohide(true),
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn render_update_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let current_version = self
            .update_service
            .as_ref()
            .map_or(env!("CARGO_PKG_VERSION"), |service| {
                service.current_version()
            });
        let checking = matches!(self.update_state, UpdateUiState::Checking);
        let progress = *self.update_progress.lock();
        let update = update_from_state(&self.update_state).cloned();

        let status_text = match &self.update_state {
            UpdateUiState::Idle if self.update_service.is_none() => {
                "更新检查组件不可用；应用其他功能不受影响。".to_string()
            }
            UpdateUiState::Idle => "尚未检查更新。".to_string(),
            UpdateUiState::Checking => "正在检查 GitHub Release…".to_string(),
            UpdateUiState::UpToDate { latest_version } => {
                format!("已是最新版本（{latest_version}）")
            }
            UpdateUiState::Available(update) => {
                format!("发现新版本 {}。", update.release.version)
            }
            UpdateUiState::UnsupportedPlatform(update) => format!(
                "发现新版本 {}，但当前平台没有对应安装包。",
                update.release.version
            ),
            UpdateUiState::Error(error) => format!("检查失败：{error}"),
        };

        v_flex()
            .w_full()
            .gap(px(16.0))
            .child(
                settings_card("版本信息", border)
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap(px(16.0))
                            .child(
                                v_flex()
                                    .gap(px(4.0))
                                    .child(
                                        div()
                                            .text_sm()
                                            .child(format!("当前版本：{current_version}")),
                                    )
                                    .child(
                                        div().text_xs().text_color(muted).child(
                                            "更新通道：稳定版；自动检查最多每 24 小时一次。",
                                        ),
                                    ),
                            )
                            .child(
                                crate::clickable_button("settings-update-check")
                                    .outline()
                                    .small()
                                    .label(if checking {
                                        "检查中…"
                                    } else {
                                        "检查更新"
                                    })
                                    .disabled(
                                        checking
                                            || self.update_downloading
                                            || self.update_service.is_none(),
                                    )
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.check_for_updates(cx);
                                    })),
                            ),
                    )
                    .child(div().text_sm().text_color(muted).child(status_text)),
            )
            .when_some(update, |page, update| {
                let release_url = update.release.release_url.clone();
                let has_asset = update.asset.is_some();
                let notes = release_notes_preview(&update);
                page.child(
                    settings_card("可用更新", border)
                        .child(
                            v_flex()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .text_sm()
                                        .child(format!("Ramag {}", update.release.version)),
                                )
                                .when_some(update.release.published_at.clone(), |section, date| {
                                    section.child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(format!("发布时间：{date}")),
                                    )
                                }),
                        )
                        .when(!notes.is_empty(), |card| {
                            card.child(div().text_sm().text_color(muted).child(notes))
                        })
                        .when(self.update_downloading, |card| {
                            card.child(
                                v_flex()
                                    .gap(px(4.0))
                                    .child(div().text_sm().child(progress_label(progress)))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child("下载完成后会自动校验 SHA-256。"),
                                    ),
                            )
                        })
                        .when_some(self.update_download_error.clone(), |card, error| {
                            card.child(div().text_sm().text_color(gpui::red()).child(error))
                        })
                        .when_some(self.update_downloaded_path.clone(), |card, path| {
                            card.child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("已校验：{}", path.display())),
                            )
                        })
                        .child(
                            h_flex()
                                .w_full()
                                .gap(px(8.0))
                                .child(
                                    crate::clickable_button("settings-update-release-page")
                                        .outline()
                                        .small()
                                        .label("打开发布页")
                                        .on_click(move |_: &ClickEvent, _, cx| {
                                            cx.open_url(&release_url);
                                        }),
                                )
                                .when(
                                    has_asset
                                        && !self.update_downloading
                                        && self.update_downloaded_path.is_none(),
                                    |row| {
                                        row.child(
                                            crate::clickable_button("settings-update-download")
                                                .primary()
                                                .small()
                                                .icon(crate::icons::download())
                                                .label("下载并校验")
                                                .on_click(cx.listener(
                                                    |this, _: &ClickEvent, _, cx| {
                                                        this.start_update_download(cx);
                                                    },
                                                )),
                                        )
                                    },
                                )
                                .when(self.update_downloading, |row| {
                                    let cancelling = self
                                        .update_cancellation
                                        .as_ref()
                                        .is_some_and(UpdateCancellation::is_cancelled);
                                    row.child(
                                        crate::clickable_button("settings-update-cancel")
                                            .danger()
                                            .small()
                                            .label(if cancelling {
                                                "正在取消…"
                                            } else {
                                                "取消"
                                            })
                                            .disabled(cancelling)
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, _, cx| {
                                                    this.cancel_update_download(cx);
                                                },
                                            )),
                                    )
                                })
                                .when(self.update_downloaded_path.is_some(), |row| {
                                    row.child(
                                        crate::clickable_button("settings-update-reveal")
                                            .primary()
                                            .small()
                                            .label(crate::platform::file_manager_reveal_label())
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, _, cx| {
                                                    this.reveal_download(cx);
                                                },
                                            )),
                                    )
                                }),
                        )
                        .child(div().text_xs().text_color(muted).child(
                            "安装包只会下载到应用缓存并校验，不会被自动执行或替换当前程序。",
                        )),
                )
            })
            .into_any_element()
    }
}

fn update_from_state(state: &UpdateUiState) -> Option<&AvailableUpdate> {
    match state {
        UpdateUiState::Available(update) | UpdateUiState::UnsupportedPlatform(update) => {
            Some(update)
        }
        _ => None,
    }
}

fn release_notes_preview(update: &AvailableUpdate) -> String {
    const MAX_CHARS: usize = 800;
    let notes = update.release.notes.trim();
    let mut preview: String = notes.chars().take(MAX_CHARS).collect();
    if notes.chars().count() > MAX_CHARS {
        preview.push('…');
    }
    preview
}

fn progress_label(progress: DownloadProgress) -> String {
    let percentage = progress
        .downloaded
        .saturating_mul(100)
        .checked_div(progress.total)
        .unwrap_or(0);
    format!(
        "下载中：{percentage}%（{} / {}）",
        format_bytes(progress.downloaded),
        format_bytes(progress.total)
    )
}

fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    format!("{:.1} MiB", bytes as f64 / MIB)
}

#[cfg(test)]
mod tests {
    use ramag_app::AvailableUpdate;
    use ramag_domain::entities::ReleaseInfo;

    use super::release_notes_preview;

    #[test]
    fn release_notes_preview_is_bounded_on_unicode_boundary() {
        let update = AvailableUpdate {
            release: ReleaseInfo {
                version: "1.0.0".into(),
                tag_name: "v1.0.0".into(),
                release_url: "https://github.com/tools-rs/ramag/releases/tag/v1.0.0".into(),
                notes: "更".repeat(900),
                published_at: None,
                assets: Vec::new(),
            },
            asset: None,
        };
        let preview = release_notes_preview(&update);
        assert_eq!(preview.chars().count(), 801);
        assert!(preview.ends_with('…'));
    }
}
