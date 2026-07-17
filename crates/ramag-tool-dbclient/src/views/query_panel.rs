//! 多标签查询面板：顶部 TabBar + 当前 QueryTab 视图

mod drafts;
mod history;

use std::sync::Arc;

use gpui::{
    AnyView, ClickEvent, Context, Entity, InteractiveElement, IntoElement, ParentElement, Point,
    Render, ScrollHandle, SharedString, Styled, Window, div, prelude::*, px,
};

use crate::actions::NewQueryTab;
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    v_flex,
};
use parking_lot::RwLock;
use ramag_app::ConnectionService;
use ramag_domain::entities::ConnectionConfig;
use ramag_ui::{
    CloseTab, MAX_EDITOR_TABS, can_open_editor_tab,
    platform::{primary_shift_shortcut, primary_shortcut},
};

use crate::sql_completion::SchemaCache;
use crate::views::query_tab::{QueryTab, QueryTabEvent};

pub struct QueryPanel {
    service: Arc<ConnectionService>,
    /// 共享给每个 Tab 的 SQL 补全缓存
    schema_cache: Arc<RwLock<SchemaCache>>,
    /// 各个标签页
    tabs: Vec<Entity<QueryTab>>,
    /// 标签页标题
    titles: Vec<String>,
    /// 当前激活的索引
    active: usize,
    /// 当前激活的连接（同步给所有 Tab + 历史面板）
    connection: Option<ConnectionConfig>,
    /// 当前激活的默认库（点表树/schema 行后同步给所有 Tab）
    active_schema: Option<String>,
    /// SQL 编辑器显隐（cmd-e 或表树按钮切换；全局生效，新 Tab 按此初始化）
    show_editor: bool,
    /// tab bar 横向滚动句柄：tab 多到溢出时，新建后滚到末尾让新 tab 可见
    tabs_scroll: ScrollHandle,
    /// 历史弹框「填入编辑器」订阅：每次打开弹框整体替换，不随打开次数累积
    history_sub: Option<gpui::Subscription>,
    /// 每个 QueryTab 的草稿变化订阅，与 tabs 同下标。
    draft_subscriptions: Vec<gpui::Subscription>,
    /// 草稿落盘防抖代际；Arc 让会话关闭后最后一次写入仍可完成。
    draft_generation: Arc<std::sync::atomic::AtomicU64>,
    /// 同一连接的草稿写入串行化；等待锁后再验代际，保证最终落盘的一定是最新快照。
    draft_write_lock: Arc<futures::lock::Mutex<()>>,
    /// 异步读取旧草稿期间，空默认标签不应抢先覆盖持久化内容。
    draft_load_pending: bool,
    /// 异步恢复期间抑制中间态落盘。
    restoring_drafts: bool,
    /// 最近一次草稿落盘失败的原因：顶部常驻警示条展示，成功后自动清除
    pub(super) draft_persist_error: Option<String>,
}

impl QueryPanel {
    pub fn new(
        service: Arc<ConnectionService>,
        schema_cache: Arc<RwLock<SchemaCache>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            service,
            schema_cache,
            tabs: Vec::new(),
            titles: Vec::new(),
            active: 0,
            connection: None,
            active_schema: None,
            // 数据浏览 / 导出是主场景，写 SQL 走 cmd-e 或表树按钮唤出
            show_editor: false,
            tabs_scroll: ScrollHandle::new(),
            history_sub: None,
            draft_subscriptions: Vec::new(),
            draft_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            draft_write_lock: Arc::new(futures::lock::Mutex::new(())),
            draft_load_pending: false,
            restoring_drafts: false,
            draft_persist_error: None,
        };
        // 默认创建一个 Tab
        this.add_tab(window, cx);
        this
    }

    /// 设置当前连接（会同步给所有 Tab + 历史面板）
    pub fn set_connection(
        &mut self,
        conn: Option<ConnectionConfig>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connection = conn.clone();
        // 切换连接时把 active_schema 重置为新连接的 database 字段
        self.active_schema = conn
            .as_ref()
            .and_then(|c| c.database.clone())
            .filter(|s| !s.is_empty());
        for tab in self.tabs.iter() {
            tab.update(cx, |t, cx| t.set_connection(conn.clone(), cx));
        }
        self.load_persisted_drafts(window, cx);
        cx.notify();
    }

    /// 同步当前默认库到所有 Tab（避免 SQL 写裸表名报 No database selected）
    pub fn set_active_schema(&mut self, schema: Option<String>, cx: &mut Context<Self>) {
        let normalized = schema.filter(|s| !s.is_empty());
        if self.active_schema == normalized {
            return;
        }
        self.active_schema = normalized.clone();
        for tab in self.tabs.iter() {
            tab.update(cx, |t, cx| t.set_active_schema(normalized.clone(), cx));
        }
        self.schedule_draft_persist(cx);
        cx.notify();
    }

    /// 切换 SQL 编辑器显隐：所有 Tab 同步；返回切换后的可见状态供调用方更新 UI
    pub fn toggle_editor(&mut self, cx: &mut Context<Self>) -> bool {
        self.show_editor = !self.show_editor;
        let v = self.show_editor;
        for tab in self.tabs.iter() {
            tab.update(cx, |t, cx| t.set_show_editor(v, cx));
        }
        self.schedule_draft_persist(cx);
        cx.notify();
        v
    }

    fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !can_open_editor_tab(self.tabs.len()) {
            window.push_notification(
                Notification::warning(format!(
                    "查询标签已达上限（{MAX_EDITOR_TABS} 个），请先关闭不需要的标签"
                ))
                .autohide(true),
                cx,
            );
            return false;
        }
        self.draft_load_pending = false;
        // 找出未使用的最小编号（这样关闭"查询 1"再新建会重新得到"查询 1"）
        let title = {
            let mut n = 1usize;
            loop {
                let candidate = format!("查询 {n}");
                if !self.titles.iter().any(|t| t == &candidate) {
                    break candidate;
                }
                n += 1;
            }
        };
        let tab = self.build_tab(title.clone(), window, cx);
        let sub = cx.subscribe(&tab, |this: &mut Self, _, e: &QueryTabEvent, cx| {
            if matches!(e, QueryTabEvent::DraftChanged) {
                this.schedule_draft_persist(cx);
            }
        });
        self.tabs.push(tab);
        self.titles.push(title);
        self.draft_subscriptions.push(sub);
        self.active = self.tabs.len() - 1;
        // 聚焦编辑器，cmd-t 后立即可输入
        self.focus_active_editor(window, cx);
        // 大负 offset 让 tab bar 滚末尾，GPUI 自动 clamp 到 max_offset
        self.tabs_scroll
            .set_offset(Point::new(px(-99999.0), px(0.0)));
        self.schedule_draft_persist(cx);
        cx.notify();
        true
    }

    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        // 防丢稿：有手写草稿先确认（确认弹窗模态，index 在回调前不会漂移）
        let has_draft = self
            .tabs
            .get(index)
            .is_some_and(|t| t.read(cx).has_user_draft(cx));
        if has_draft {
            let entity = cx.entity();
            ramag_ui::open_confirm(
                "关闭查询标签？",
                "该标签的编辑器有未保存的手写内容，关闭将丢弃。".to_string(),
                "关闭",
                true,
                move |window, app| {
                    entity.update(app, |this, cx| this.close_tab_inner(index, window, cx));
                },
                window,
                cx,
            );
            return;
        }
        self.close_tab_inner(index, window, cx);
    }

    fn close_tab_inner(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.draft_load_pending = false;
        // 执行中的查询先取消（含后端 KILL），避免关 Tab 后语句仍占用数据库
        if let Some(tab) = self.tabs.get(index) {
            tab.update(cx, |t, cx| t.cancel_if_running(window, cx));
        }
        self.tabs.remove(index);
        self.titles.remove(index);
        if index < self.draft_subscriptions.len() {
            let _ = self.draft_subscriptions.remove(index);
        }
        // 调整 active
        if self.tabs.is_empty() {
            self.add_tab(window, cx); // 总保持至少一个 Tab（add_tab 内部会 focus）
            return;
        }
        self.active = active_index_after_close(self.active, index, self.tabs.len());
        // 关闭后让新 active tab 编辑器获得焦点，无需再点一下
        self.focus_active_editor(window, cx);
        self.schedule_draft_persist(cx);
        cx.notify();
    }

    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index < self.tabs.len() && self.active != index {
            self.draft_load_pending = false;
            self.active = index;
            self.focus_active_editor(window, cx);
            self.schedule_draft_persist(cx);
            cx.notify();
        }
    }

    /// 聚焦当前激活 Tab 的编辑器
    pub fn focus_active_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active) {
            tab.update(cx, |t, cx| t.focus_editor(window, cx));
        }
    }

    /// SQL 编辑器当前是否可见（供会话决定 Tab 激活时聚焦编辑器还是会话根）
    pub fn is_editor_visible(&self) -> bool {
        self.show_editor
    }

    /// 把 SQL 写入当前激活 Tab 并立即执行
    pub fn prefill_active_sql_and_run(
        &mut self,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft_load_pending = false;
        if let Some(tab) = self.tabs.get(self.active) {
            tab.update(cx, |t, cx| {
                t.cancel_if_running(window, cx);
                t.set_sql(sql.clone(), window, cx);
                t.mark_injected(sql);
                t.run(window, cx);
            });
        }
        self.schedule_draft_persist(cx);
    }

    /// 同 prefill_active_sql_and_run，额外注入精确目标表 (schema, table)
    /// 表树点击触发的 SELECT 用：避开反引号内带短横线被 SQL parser 吞的坑
    pub fn prefill_active_sql_and_run_with_target(
        &mut self,
        sql: String,
        target: Option<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft_load_pending = false;
        // 草稿保护：活动 Tab 存在手写内容（非空且非上次自动注入）时不覆盖，
        // 另开 Tab 浏览；未手改的浏览 SQL / 示例则原地复用，连点切表不膨胀 Tab
        let has_draft = self
            .tabs
            .get(self.active)
            .is_some_and(|t| t.read(cx).has_user_draft(cx));
        if has_draft && !self.add_tab(window, cx) {
            return;
        }
        if let Some(tab) = self.tabs.get(self.active) {
            tab.update(cx, |t, cx| {
                t.cancel_if_running(window, cx);
                // set_sql 内会清 pinned_target，所以必须先 set_sql 再 set_pinned_target
                t.set_sql(sql.clone(), window, cx);
                t.mark_injected(sql);
                t.set_pinned_target(target);
                // 切表时同步清空两个过滤框，避免旧 filter 挡新表数据
                t.clear_result_filters(window, cx);
                t.run(window, cx);
            });
        }
        self.schedule_draft_persist(cx);
    }

    /// 新建一个 Tab 写入 SQL 并立即执行（用于 SHOW CREATE TABLE 等辅助查询，
    /// 不污染用户当前编辑的 Tab）
    pub fn open_in_new_tab_and_run(
        &mut self,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.add_tab(window, cx) {
            return;
        }
        self.prefill_active_sql_and_run(sql, window, cx);
    }

    /// 活动 Tab 是否存在手写草稿（防丢稿：示例 / 历史填入前判定，有稿改道新 Tab）
    fn active_has_draft(&self, cx: &Context<Self>) -> bool {
        self.tabs
            .get(self.active)
            .is_some_and(|t| t.read(cx).has_user_draft(cx))
    }

    /// 把示例 SQL 插入当前激活 Tab 的编辑器（Tab 栏「示例」下拉用）。
    /// 有手写草稿时另开 Tab，不覆盖
    fn insert_example_into_active(
        &mut self,
        sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft_load_pending = false;
        if self.active_has_draft(cx) && !self.add_tab(window, cx) {
            return;
        }
        if let Some(tab) = self.tabs.get(self.active).cloned() {
            tab.update(cx, |t, cx| t.insert_example(sql, window, cx));
        }
        self.schedule_draft_persist(cx);
    }

    /// 把 SQL 填入当前活动 Tab 的编辑器并聚焦（不执行）。
    /// 有手写草稿时另开 Tab，不覆盖；面板恒保持至少一个 Tab，空列表仅是兜底防御
    fn fill_active_sql(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        self.draft_load_pending = false;
        let needs_new_tab = self.tabs.is_empty() || self.active_has_draft(cx);
        if needs_new_tab && !self.add_tab(window, cx) {
            return;
        }
        if let Some(tab) = self.tabs.get(self.active) {
            tab.update(cx, |t, cx| {
                t.set_sql(sql, window, cx);
                t.focus_editor(window, cx);
            });
        }
        self.schedule_draft_persist(cx);
        cx.notify();
    }
}

impl Render for QueryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let border = theme.border;
        let secondary_bg = theme.secondary;
        let muted_bg = theme.muted;
        let accent = theme.accent;

        let active = self.active;
        // 优先用 QueryTab 的 display_title（执行后变 SQL 摘要），fallback 到默认 titles
        let titles: Vec<String> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let dt = t.read(cx).display_title().to_string();
                if dt.is_empty() {
                    self.titles.get(i).cloned().unwrap_or_default()
                } else {
                    dt
                }
            })
            .collect();
        let only_one = titles.len() <= 1;
        let can_add_tab = can_open_editor_tab(self.tabs.len());

        // 当前主区视图：始终是 active Tab
        let current_view: Option<AnyView> = self.tabs.get(active).map(|t| t.clone().into());

        // Tab Bar 渲染
        let tab_bar_items: Vec<gpui::AnyElement> = titles
            .iter()
            .enumerate()
            .map(|(idx, title)| {
                let is_active = idx == active;
                let title = title.clone();
                let id_select = SharedString::from(format!("tab-{idx}"));
                let id_close = SharedString::from(format!("tab-close-{idx}"));

                let mut tab = h_flex()
                    .id(id_select)
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py(px(7.0))
                    .border_r_1()
                    .border_color(border)
                    .cursor_pointer()
                    .child(
                        div()
                            .text_xs()
                            .text_color(if is_active { fg } else { muted_fg })
                            .child(title),
                    )
                    .when(!only_one, |tab| {
                        tab.child(
                            Button::new(id_close)
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.close_tab(idx, window, cx);
                                })),
                        )
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.select_tab(idx, window, cx);
                    }));

                if is_active {
                    tab = tab.bg(theme_active_bg(secondary_bg, accent));
                } else {
                    tab = tab.hover(move |this| this.bg(muted_bg));
                }

                tab.into_any_element()
            })
            .collect();

        v_flex()
            .size_full()
            .key_context("QueryPanel")
            // 监听 NewQueryTab / CloseTab，绑定见 main.rs
            .on_action(cx.listener(|this, _: &NewQueryTab, window, cx| {
                this.add_tab(window, cx);
            }))
            // 草稿落盘失败常驻警示：用户以为可跨重启恢复，静默失败等于丢稿
            .when_some(self.draft_persist_error.clone(), |panel, err| {
                let warning = theme.warning;
                let mut warn_bg = warning;
                warn_bg.a = 0.12;
                panel.child(
                    h_flex()
                        .w_full()
                        .flex_none()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py(px(5.0))
                        .bg(warn_bg)
                        .border_b_1()
                        .border_color(border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(warning)
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(format!(
                                    "⚠ 草稿自动保存失败：{err}（草稿可能无法跨重启恢复，请复制重要内容备份）"
                                )),
                        )
                        .child(
                            Button::new("draft-persist-retry")
                                .ghost()
                                .small()
                                .label("重试")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.schedule_draft_persist(cx);
                                })),
                        ),
                )
            })
            // 始终关闭当前查询标签；最后一个关闭后会立即补一个空白标签。
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if !this.tabs.is_empty() {
                    let idx = this.active;
                    this.close_tab(idx, window, cx);
                } else {
                    cx.propagate();
                }
            }))
            // Tab Bar 仅在 SQL 编辑器可见时渲染
            .when(self.show_editor, |panel| {
                panel.child(
                    h_flex()
                        .w_full()
                        .flex_none()
                        .border_b_1()
                        .border_color(border)
                        .bg(secondary_bg)
                        // 左：tabs 区，溢出时横向滚动；min_w_0 让它能被压缩
                        .child(
                            h_flex()
                                .id("query-tabs-scroll")
                                .flex_1()
                                .min_w_0()
                                .overflow_x_scroll()
                                .track_scroll(&self.tabs_scroll)
                                .children(tab_bar_items)
                                // + 新建按钮跟在最后一个 tab 之后
                                .child(
                                    Button::new("tab-add")
                                        .ghost()
                                        .small()
                                        .icon(IconName::Plus)
                                        .tooltip(if can_add_tab {
                                            format!("新建查询 ({})", primary_shortcut("T"))
                                        } else {
                                            format!("查询标签已达上限（{MAX_EDITOR_TABS} 个）")
                                        })
                                        .disabled(!can_add_tab)
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.add_tab(window, cx);
                                            },
                                        )),
                                ),
                        )
                        // 右：示例 / 格式化 / EXPLAIN / 历史（弹框）
                        .child(
                            h_flex()
                                .flex_none()
                                .items_center()
                                .border_l_1()
                                .border_color(border)
                                .child({
                                    let entity = cx.entity();
                                    let driver = self.connection.as_ref().map(|c| c.driver);
                                    // 模板个性化：用当前 Tab 最近点开的表名，没有则占位
                                    let table = self
                                        .tabs
                                        .get(self.active)
                                        .and_then(|t| {
                                            t.read(cx)
                                                .pinned_target
                                                .as_ref()
                                                .map(|(_, table)| table.clone())
                                        })
                                        .unwrap_or_default();
                                    Button::new("sql-examples")
                                        .ghost()
                                        .small()
                                        .icon(ramag_ui::icons::scroll_text())
                                        .tooltip("常用 SQL 示例（插入编辑器）")
                                        .disabled(driver.is_none())
                                        .dropdown_menu(move |menu, _, _| {
                                            let Some(driver) = driver else {
                                                return menu;
                                            };
                                            let mut m = menu;
                                            for (label, sql) in
                                                super::query_tab::sql_examples(driver, &table)
                                            {
                                                let e = entity.clone();
                                                m = m.item(PopupMenuItem::new(label).on_click(
                                                    move |_, window, app| {
                                                        e.update(app, |panel, cx| {
                                                            panel.insert_example_into_active(
                                                                &sql, window, cx,
                                                            );
                                                        });
                                                    },
                                                ));
                                            }
                                            m
                                        })
                                })
                                .child(
                                    Button::new("format-sql")
                                        .ghost()
                                        .small()
                                        .icon(ramag_ui::icons::wand_sparkles())
                                        .tooltip(format!(
                                            "美化 SQL ({})",
                                            primary_shift_shortcut("F")
                                        ))
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                if let Some(tab) =
                                                    this.tabs.get(this.active).cloned()
                                                {
                                                    tab.update(cx, |t, cx| {
                                                        t.handle_format(window, cx)
                                                    });
                                                }
                                            },
                                        )),
                                )
                                .child(
                                    Button::new("explain-sql")
                                        .ghost()
                                        .small()
                                        .icon(ramag_ui::icons::gauge())
                                        .tooltip(format!(
                                            "执行计划 EXPLAIN ({})",
                                            primary_shift_shortcut("E")
                                        ))
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                if let Some(tab) =
                                                    this.tabs.get(this.active).cloned()
                                                {
                                                    tab.update(cx, |t, cx| {
                                                        t.handle_explain(window, cx)
                                                    });
                                                }
                                            },
                                        )),
                                )
                                .child(
                                    // 上游 IconName 无 History 变体，用旧版历史入口同款日历图标
                                    Button::new("query-history")
                                        .ghost()
                                        .small()
                                        .icon(IconName::Calendar)
                                        .tooltip("查询历史")
                                        .disabled(self.connection.is_none())
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.open_history_dialog(window, cx);
                                            },
                                        )),
                                ),
                        ),
                )
            })
            // 当前 Tab 内容
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when_some(current_view, |this, view| this.child(view)),
            )
    }
}

/// 选中 Tab 的背景色：在 secondary 上叠加微弱 accent
fn theme_active_bg(_secondary: gpui::Hsla, accent: gpui::Hsla) -> gpui::Hsla {
    let mut a = accent;
    a.a = 0.15;
    a
}

fn active_index_after_close(active: usize, closed: usize, remaining: usize) -> usize {
    debug_assert!(remaining > 0);
    if active >= remaining {
        remaining - 1
    } else if active > closed {
        active - 1
    } else {
        active
    }
}

#[cfg(test)]
mod tests {
    use super::active_index_after_close;

    #[test]
    fn closing_tab_left_of_active_preserves_the_same_logical_tab() {
        assert_eq!(active_index_after_close(1, 0, 2), 0);
    }

    #[test]
    fn closing_active_last_tab_activates_new_last_tab() {
        assert_eq!(active_index_after_close(2, 2, 2), 1);
    }

    #[test]
    fn closing_tab_right_of_active_keeps_index() {
        assert_eq!(active_index_after_close(0, 2, 2), 0);
    }
}
