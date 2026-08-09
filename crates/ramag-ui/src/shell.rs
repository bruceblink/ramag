//! 主壳：原生 TitleBar + 左 ActivityBar（52px）+ 右 Tool/HomeView。视图由外部 register_tool_view 注入

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyView, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window,
    div, prelude::*,
};
use gpui_component::{ActiveTheme, Root, h_flex, v_flex};
use ramag_app::{DataSyncGate, ToolRegistry};

use crate::activity_bar::{ActivityBar, NavEvent, NavTarget};

pub struct Shell {
    activity_bar: Entity<ActivityBar>,
    data_sync_gate: Arc<DataSyncGate>,
    data_sync_overlay: Entity<crate::DataSyncOverlay>,
    /// 保留注册表以按 tool_id 解析工具名（窗口标题用）
    registry: Arc<ToolRegistry>,
    tool_views: HashMap<String, AnyView>,
    home_view: Option<AnyView>,
    settings_view: Option<AnyView>,
    /// None=首页，Some(tool_id)=某工具
    selected: Option<String>,
    settings_selected: bool,
    /// 窗口 bounds 持久化防抖代际：拖动 / 缩放高频回调，停顿后才落盘
    bounds_gen: u64,

    _subscriptions: Vec<Subscription>,
}

/// 窗口位置尺寸偏好（prefs key `window_bounds`）；重启按此恢复
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct WindowBoundsPref {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub maximized: bool,
}

impl WindowBoundsPref {
    pub const PREF_KEY: &'static str = "window_bounds";
    const MAX_PREF_BYTES: usize = 1024;
    const MAX_ABS_COORDINATE: f32 = 1_000_000.0;
    const MAX_SIZE: f32 = 1_000_000.0;

    pub fn parse(json: &str) -> Result<Self, String> {
        if json.len() > Self::MAX_PREF_BYTES {
            return Err(format!("窗口位置数据过大：{} bytes", json.len()));
        }
        let pref: Self =
            serde_json::from_str(json).map_err(|error| format!("窗口位置数据格式无效：{error}"))?;
        if !pref.x.is_finite() || !pref.y.is_finite() || !pref.w.is_finite() || !pref.h.is_finite()
        {
            return Err("窗口位置与尺寸必须为有限数值".into());
        }
        if pref.x.abs() > Self::MAX_ABS_COORDINATE || pref.y.abs() > Self::MAX_ABS_COORDINATE {
            return Err("窗口坐标超出安全范围".into());
        }
        if pref.w <= 0.0 || pref.h <= 0.0 || pref.w > Self::MAX_SIZE || pref.h > Self::MAX_SIZE {
            return Err("窗口尺寸超出安全范围".into());
        }
        Ok(pref)
    }
}

impl Shell {
    pub fn new(
        registry: Arc<ToolRegistry>,
        data_sync_gate: Arc<DataSyncGate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let activity_bar = cx.new(|cx| ActivityBar::new(registry.clone(), cx));
        let data_sync_overlay =
            cx.new(|cx| crate::DataSyncOverlay::new(data_sync_gate.clone(), cx));
        let registry_for_title = registry.clone();

        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(
            &activity_bar,
            window,
            |this, _, event: &NavEvent, window, cx| match event {
                NavEvent::Navigate(target) => {
                    this.handle_navigate(target.clone(), window, cx);
                }
            },
        ));
        // 窗口移动 / 缩放 → 防抖持久化位置尺寸（重启恢复）
        subs.push(cx.observe_window_bounds(window, |this, window, cx| {
            this.schedule_persist_bounds(window, cx);
        }));

        Self {
            activity_bar,
            data_sync_gate,
            data_sync_overlay,
            registry: registry_for_title,
            tool_views: HashMap::new(),
            home_view: None,
            settings_view: None,
            selected: None,
            settings_selected: false,
            bounds_gen: 0,
            _subscriptions: subs,
        }
    }

    /// 窗口标题：首页 `Ramag`，工具页 `Ramag — 工具名`。
    /// 保留可辨识标题（Windows 任务栏 / 窗口列表），不置空
    fn window_title(&self) -> String {
        if self.settings_selected {
            return "Ramag — 设置".to_string();
        }
        match &self.selected {
            None => "Ramag".to_string(),
            Some(id) => self
                .registry
                .find(id)
                .map(|t| format!("Ramag — {}", t.meta().name))
                .unwrap_or_else(|| "Ramag".to_string()),
        }
    }

    /// 防抖 600ms 落盘窗口 bounds：拖动期间高频回调只取最终静止值。
    /// 最大化时不覆盖记录的普通尺寸（仅更新 maximized 标记），取消最大化能回原位
    fn schedule_persist_bounds(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(storage) = crate::theme::storage_from_cx(cx) else {
            return;
        };
        self.bounds_gen = self.bounds_gen.wrapping_add(1);
        let generation = self.bounds_gen;
        let maximized = window.is_maximized();
        let b = window.bounds();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(600))
                .await;
            let fresh = this
                .update(cx, |this, _| this.bounds_gen == generation)
                .unwrap_or(false);
            if !fresh {
                return;
            }
            // 最大化态：读回已存普通尺寸，仅翻 maximized 位（没有存过则记当前值兜底）
            let pref = if maximized {
                let existing = match storage.get_preference(WindowBoundsPref::PREF_KEY).await {
                    Ok(Some(json)) => match WindowBoundsPref::parse(&json) {
                    Ok(pref) => Some(pref),
                    Err(error) => {
                        tracing::warn!(
                            operation = "window_bounds_load",
                            error,
                            "ignore invalid saved window bounds"
                        );
                            None
                        }
                    },
                    Ok(None) => None,
                    Err(error) => {
                        tracing::warn!(operation = "window_bounds_save", stage = "load_before_maximize", error = %error, "load window bounds before maximize failed");
                        None
                    }
                };
                match existing {
                    Some(mut p) => {
                        p.maximized = true;
                        p
                    }
                    None => WindowBoundsPref {
                        x: f32::from(b.origin.x),
                        y: f32::from(b.origin.y),
                        w: f32::from(b.size.width),
                        h: f32::from(b.size.height),
                        maximized: true,
                    },
                }
            } else {
                WindowBoundsPref {
                    x: f32::from(b.origin.x),
                    y: f32::from(b.origin.y),
                    w: f32::from(b.size.width),
                    h: f32::from(b.size.height),
                    maximized: false,
                }
            };
            let json = match serde_json::to_string(&pref) {
                Ok(json) => json,
                Err(error) => {
                    tracing::warn!(operation = "window_bounds_save", stage = "serialize", error = %error, "serialize window bounds failed");
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                if this.bounds_gen != generation {
                    return;
                }
                crate::preferences::persist_preference_latest_with_storage(
                    WindowBoundsPref::PREF_KEY,
                    json,
                    storage,
                    cx,
                );
            });
        })
        .detach();
    }

    pub fn set_home_view(&mut self, view: AnyView) {
        self.home_view = Some(view);
    }

    pub fn set_settings_view(&mut self, view: AnyView) {
        self.settings_view = Some(view);
    }

    pub fn register_tool_view(&mut self, tool_id: impl Into<String>, view: AnyView) {
        self.tool_views.insert(tool_id.into(), view);
    }

    pub fn retain_subscription(&mut self, subscription: Subscription) {
        self._subscriptions.push(subscription);
    }

    /// 程序内导航
    pub fn navigate_to(&mut self, target: NavTarget, window: &mut Window, cx: &mut Context<Self>) {
        if self.data_sync_gate.is_blocking() {
            return;
        }
        self.activity_bar
            .update(cx, |bar, cx| bar.set_selected(target.clone(), cx));
        self.handle_navigate(target, window, cx);
    }

    fn handle_navigate(&mut self, target: NavTarget, window: &mut Window, cx: &mut Context<Self>) {
        if self.data_sync_gate.is_blocking() {
            return;
        }
        let (new_selected, settings_selected) = match target {
            NavTarget::Home => (None, false),
            NavTarget::Tool(id) => (Some(id), false),
            NavTarget::Settings => (None, true),
        };

        if self.selected != new_selected || self.settings_selected != settings_selected {
            self.selected = new_selected;
            self.settings_selected = settings_selected;
            // 记住停留位置：下次启动直接回到该工具（重启不回炉 Home）
            if !self.settings_selected {
                persist_last_tool(self.selected.clone(), cx);
            }
            cx.notify();
        }
        // 标题始终反映当前工具（含首帧 / 恢复到某工具），保留任务栏可辨识性
        window.set_window_title(&self.window_title());
    }

    /// 跳到第 n 个已注册工具（0-based）；越界忽略。Cmd/Ctrl+1/2/3 用
    fn select_tool_index(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tool) = self.registry.list().get(n) {
            self.navigate_to(NavTarget::Tool(tool.meta().id.clone()), window, cx);
        }
    }

    /// 在「首页 + 各工具 + 设置」间循环；`reverse=true` 时反向。
    fn cycle_section(&mut self, reverse: bool, window: &mut Window, cx: &mut Context<Self>) {
        let tools = self.registry.list();
        let section_count = tools.len() + 2;
        let current = if self.settings_selected {
            section_count - 1
        } else if let Some(id) = &self.selected {
            tools
                .iter()
                .position(|tool| tool.meta().id == *id)
                .map_or(0, |index| index + 1)
        } else {
            0
        };
        let next = if reverse {
            (current + section_count - 1) % section_count
        } else {
            (current + 1) % section_count
        };
        if next == 0 {
            self.navigate_to(NavTarget::Home, window, cx);
        } else if next == section_count - 1 {
            self.navigate_to(NavTarget::Settings, window, cx);
        } else {
            self.select_tool_index(next - 1, window, cx);
        }
    }
}

/// 上次工具落 prefs（后台异步，失败仅告警）。Home 存空串
fn persist_last_tool(selected: Option<String>, cx: &mut gpui::App) {
    let value = selected.unwrap_or_default();
    crate::preferences::persist_preference_latest("last_tool", value, cx);
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 先拷颜色避开 theme 借用与 cx 可变借用冲突
        let bg_color = cx.theme().background;
        let fg_color = cx.theme().foreground;

        let content_view: Option<AnyView> = if self.settings_selected {
            self.settings_view.clone()
        } else {
            match &self.selected {
                None => self.home_view.clone(),
                Some(id) => self.tool_views.get(id).cloned(),
            }
        };

        // dialog / notification 浮层须由顶层 view 渲染
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);

        v_flex()
            .size_full()
            .bg(bg_color)
            .text_color(fg_color)
            .key_context("Shell")
            // 工具切换快捷键：Cmd/Ctrl+1/2/3/4 跳工具，Ctrl+Tab 循环区段
            .on_action(
                cx.listener(|this, _: &crate::actions::SelectTool1, window, cx| {
                    this.select_tool_index(0, window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::SelectTool2, window, cx| {
                    this.select_tool_index(1, window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::SelectTool3, window, cx| {
                    this.select_tool_index(2, window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::SelectTool4, window, cx| {
                    this.select_tool_index(3, window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::CycleSection, window, cx| {
                    this.cycle_section(false, window, cx);
                }),
            )
            .on_action(cx.listener(
                |this, _: &crate::actions::CycleSectionReverse, window, cx| {
                    this.cycle_section(true, window, cx);
                },
            ))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.activity_bar.clone())
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .when_some(content_view, |this, view| this.child(view))
                            .when(
                                (self.settings_selected && self.settings_view.is_none())
                                    || (!self.settings_selected
                                        && self.selected.is_some()
                                        && self
                                            .selected
                                            .as_ref()
                                            .and_then(|id| self.tool_views.get(id))
                                            .is_none()),
                                |this| this.child(render_view_missing(cx)),
                            ),
                    ),
            )
            .children(dialog_layer)
            .children(notification_layer)
            .child(self.data_sync_overlay.clone())
    }
}

fn render_view_missing(cx: &Context<Shell>) -> impl IntoElement {
    let theme = cx.theme();
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(div().text_lg().child("视图未注册"))
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("请检查 ramag-bin/main.rs 是否调用了 register_tool_view"),
        )
}

#[cfg(test)]
mod tests {
    use super::WindowBoundsPref;

    #[test]
    fn window_bounds_reject_invalid_values() {
        assert!(
            WindowBoundsPref::parse(
                r#"{"x":10.0,"y":20.0,"w":1200.0,"h":780.0,"maximized":false}"#
            )
            .is_ok()
        );
        assert!(
            WindowBoundsPref::parse(r#"{"x":10.0,"y":20.0,"w":-1.0,"h":780.0,"maximized":false}"#)
                .is_err()
        );
        assert!(
            WindowBoundsPref::parse(
                r#"{"x":1000001.0,"y":20.0,"w":1200.0,"h":780.0,"maximized":false}"#
            )
            .is_err()
        );
        assert!(
            WindowBoundsPref::parse(&" ".repeat(WindowBoundsPref::MAX_PREF_BYTES + 1)).is_err()
        );
    }
}
