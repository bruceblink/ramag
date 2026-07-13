//! 主壳：原生 TitleBar + 左 ActivityBar（52px）+ 右 Tool/HomeView。视图由外部 register_tool_view 注入

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyView, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window,
    div, prelude::*,
};
use gpui_component::{ActiveTheme, Root, h_flex, v_flex};
use ramag_app::ToolRegistry;

use crate::activity_bar::{ActivityBar, NavEvent, NavTarget};

pub struct Shell {
    activity_bar: Entity<ActivityBar>,
    /// 保留注册表以按 tool_id 解析工具名（窗口标题用）
    registry: Arc<ToolRegistry>,
    tool_views: HashMap<String, AnyView>,
    home_view: Option<AnyView>,
    /// 首页的强类型句柄（同 crate）：菜单「重新查看快速上手」经此重开引导
    home_entity: Option<Entity<crate::home_view::HomeView>>,
    /// None=首页，Some(tool_id)=某工具
    selected: Option<String>,
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

    pub fn parse(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }
}

impl Shell {
    pub fn new(registry: Arc<ToolRegistry>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let activity_bar = cx.new(|_| ActivityBar::new(registry.clone()));
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
        // 跟随系统主题：用户显式选过则忽略
        subs.push(cx.observe_window_appearance(window, |_this, window, cx| {
            crate::theme::on_system_appearance_changed(window.appearance(), cx);
        }));
        // 窗口移动 / 缩放 → 防抖持久化位置尺寸（重启恢复）
        subs.push(cx.observe_window_bounds(window, |this, window, cx| {
            this.schedule_persist_bounds(window, cx);
        }));

        Self {
            activity_bar,
            registry: registry_for_title,
            tool_views: HashMap::new(),
            home_view: None,
            home_entity: None,
            selected: None,
            bounds_gen: 0,
            _subscriptions: subs,
        }
    }

    /// 窗口标题：首页 `Ramag`，工具页 `Ramag — 工具名`。
    /// 保留可辨识标题（Windows 任务栏 / 窗口列表），不置空
    fn window_title(&self) -> String {
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
                let existing = storage
                    .get_preference(WindowBoundsPref::PREF_KEY)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|j| WindowBoundsPref::parse(&j));
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
            let Ok(json) = serde_json::to_string(&pref) else {
                return;
            };
            if let Err(e) = storage
                .set_preference(WindowBoundsPref::PREF_KEY, &json)
                .await
            {
                tracing::warn!(error = %e, "persist window bounds failed");
            }
        })
        .detach();
    }

    pub fn set_home_view(&mut self, view: AnyView) {
        self.home_view = Some(view);
    }

    /// 注入首页强类型句柄（同 crate）：菜单「重新查看快速上手」经此重开引导
    pub fn set_home_entity(&mut self, entity: Entity<crate::home_view::HomeView>) {
        self.home_entity = Some(entity);
    }

    pub fn register_tool_view(&mut self, tool_id: impl Into<String>, view: AnyView) {
        self.tool_views.insert(tool_id.into(), view);
    }

    /// 程序内导航
    pub fn navigate_to(&mut self, target: NavTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.activity_bar
            .update(cx, |bar, cx| bar.set_selected(target.clone(), cx));
        self.handle_navigate(target, window, cx);
    }

    fn handle_navigate(&mut self, target: NavTarget, window: &mut Window, cx: &mut Context<Self>) {
        let new_selected = match target {
            NavTarget::Home => None,
            NavTarget::Tool(id) => Some(id),
        };

        if self.selected != new_selected {
            self.selected = new_selected;
            // 记住停留位置：下次启动直接回到该工具（重启不回炉 Home）
            persist_last_tool(self.selected.clone(), cx);
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

    /// 在「首页 + 各工具」区段间循环切换（Ctrl+Tab）：当前区段的下一个，末尾回到首页
    fn cycle_section(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tools = self.registry.list();
        // 区段序列：[Home, tool0, tool1, ...]，用 Option<usize> 表达（None=Home）
        let cur = self
            .selected
            .as_ref()
            .and_then(|id| tools.iter().position(|t| t.meta().id == *id));
        let next = match cur {
            None => tools.first().map(|_| 0),        // Home → 第一个工具
            Some(i) if i + 1 < tools.len() => Some(i + 1), // 下一个工具
            Some(_) => None,                          // 最后一个工具 → 回首页
        };
        match next {
            Some(i) => self.select_tool_index(i, window, cx),
            None => self.navigate_to(NavTarget::Home, window, cx),
        }
    }
}

/// 上次工具落 prefs（后台异步，失败仅告警）。Home 存空串
fn persist_last_tool(selected: Option<String>, cx: &mut gpui::App) {
    let Some(storage) = crate::theme::storage_from_cx(cx) else {
        return;
    };
    let value = selected.unwrap_or_default();
    cx.background_executor()
        .spawn(async move {
            if let Err(e) = storage.set_preference("last_tool", &value).await {
                tracing::warn!(error = %e, "persist last tool failed");
            }
        })
        .detach();
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 先拷颜色避开 theme 借用与 cx 可变借用冲突
        let bg_color = cx.theme().background;
        let fg_color = cx.theme().foreground;

        let content_view: Option<AnyView> = match &self.selected {
            None => self.home_view.clone(),
            Some(id) => self.tool_views.get(id).cloned(),
        };

        // dialog / notification 浮层须由顶层 view 渲染
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);

        v_flex()
            .size_full()
            .bg(bg_color)
            .text_color(fg_color)
            .key_context("Shell")
            // 工具切换快捷键：Cmd/Ctrl+1/2/3 跳工具，Ctrl+Tab 循环区段
            .on_action(cx.listener(|this, _: &crate::actions::SelectTool1, window, cx| {
                this.select_tool_index(0, window, cx);
            }))
            .on_action(cx.listener(|this, _: &crate::actions::SelectTool2, window, cx| {
                this.select_tool_index(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &crate::actions::SelectTool3, window, cx| {
                this.select_tool_index(2, window, cx);
            }))
            .on_action(cx.listener(|this, _: &crate::actions::CycleSection, window, cx| {
                this.cycle_section(window, cx);
            }))
            // 菜单「重新查看快速上手」：切回首页并重开引导卡片
            .on_action(cx.listener(|this, _: &crate::actions::ShowOnboarding, window, cx| {
                this.navigate_to(NavTarget::Home, window, cx);
                if let Some(home) = this.home_entity.clone() {
                    home.update(cx, |h, cx| h.reshow_onboarding(cx));
                }
            }))
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
                                self.selected.is_some()
                                    && self
                                        .selected
                                        .as_ref()
                                        .and_then(|id| self.tool_views.get(id))
                                        .is_none(),
                                |this| this.child(render_view_missing(cx)),
                            ),
                    ),
            )
            .children(dialog_layer)
            .children(notification_layer)
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
