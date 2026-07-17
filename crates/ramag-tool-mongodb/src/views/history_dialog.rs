//! MongoDB 查询历史弹框：与 SQL 共用同一张历史表（sql 字段存原始 JSON 命令）。
//! 搜索 / 复制 / 填入编辑器 / 重跑 / 删除 / 清空；入口在 MongoQueryPanel 工具条

use std::sync::Arc;
use std::{ops::Range, rc::Rc};

use gpui::{
    AnyElement, ClickEvent, ClipboardItem, Context, EventEmitter, Hsla, IntoElement, ParentElement,
    Render, SharedString, Styled, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    v_flex,
};
use ramag_app::MongoService;
use ramag_domain::entities::{
    ConnectionId, QueryRecord, QueryRecordId, QueryStatus, compact_text_preview,
};
use tracing::error;

/// 单次最多加载条数
const HISTORY_LIMIT: usize = 200;
/// 命令单行预览最大字符数
const PREVIEW_MAX_CHARS: usize = 160;
/// 失败原因展示最大字符数
const ERROR_MAX_CHARS: usize = 80;
/// 列表区固定高度
const LIST_HEIGHT: f32 = 480.0;

/// 历史中心行为事件：MongoQueryPanel 订阅处理
pub enum MongoHistoryEvent {
    /// 填入当前活动 Tab 的命令编辑器（不执行）
    FillEditor(String),
    /// 填入并立即执行（重跑 / 失败重试）
    RunCommand(String),
}

/// 弹框内容视图：异步加载 + 搜索过滤 + 删除 / 清空
pub struct MongoHistoryList {
    service: Arc<MongoService>,
    connection_id: ConnectionId,
    records: Vec<Arc<QueryRecord>>,
    history_truncated: bool,
    loading: bool,
    /// 删除 / 清空串行执行，避免重复点击与旧加载结果在清空后回写。
    mutating: bool,
    load_error: Option<String>,
    mutation_error: Option<String>,
    search: gpui::Entity<InputState>,
}

impl EventEmitter<MongoHistoryEvent> for MongoHistoryList {}

impl MongoHistoryList {
    pub fn new(
        service: Arc<MongoService>,
        connection_id: ConnectionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx)
                .placeholder("搜索命令 / 错误内容")
                .clean_on_escape()
        });
        cx.subscribe(&search, |_this: &mut Self, _, _e: &InputEvent, cx| {
            cx.notify()
        })
        .detach();
        let mut this = Self {
            service,
            connection_id,
            records: Vec::new(),
            history_truncated: false,
            loading: true,
            mutating: false,
            load_error: None,
            mutation_error: None,
            search,
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.load_error = None;
        let svc = self.service.clone();
        let conn_id = self.connection_id.clone();
        cx.spawn(async move |this, cx| {
            let result = svc.list_history(Some(&conn_id), HISTORY_LIMIT).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(page) => {
                        this.history_truncated = page.truncated;
                        this.records = page.records.into_iter().map(Arc::new).collect();
                    }
                    Err(e) => {
                        error!(error = %e, "load mongo history failed");
                        this.load_error = Some(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn delete_record(&mut self, id: QueryRecordId, cx: &mut Context<Self>) {
        if !self.begin_mutation(cx) {
            return;
        }
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let r = svc.delete_history(&id).await;
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match r {
                    Ok(()) => this.records.retain(|rec| rec.id != id),
                    Err(e) => {
                        error!(error = %e, "delete mongo history failed");
                        this.mutation_error = Some(format!("删除失败：{e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn clear_all(&mut self, cx: &mut Context<Self>) {
        if !self.begin_mutation(cx) {
            return;
        }
        let svc = self.service.clone();
        let conn_id = self.connection_id.clone();
        cx.spawn(async move |this, cx| {
            let r = svc.clear_history(Some(&conn_id)).await;
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match r {
                    Ok(()) => {
                        this.records.clear();
                        this.history_truncated = false;
                    }
                    Err(e) => {
                        error!(error = %e, "clear mongo history failed");
                        this.mutation_error = Some(format!("清空失败：{e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn begin_mutation(&mut self, cx: &mut Context<Self>) -> bool {
        if self.loading || self.mutating {
            return false;
        }
        self.mutating = true;
        self.mutation_error = None;
        cx.notify();
        true
    }

    fn render_row(&self, ix: usize, rec: Arc<QueryRecord>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted_fg = theme.muted_foreground;
        let border = theme.border;
        let muted_bg = theme.muted;
        let status_color = match rec.status {
            QueryStatus::Success => theme.success,
            QueryStatus::Failed => theme.danger,
        };
        let preview = rec.sql_preview(PREVIEW_MAX_CHARS);
        let when_text = rec
            .executed_at
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let mut meta = match rec.status {
            QueryStatus::Success => {
                format!("{} · {} 行 · {} ms", when_text, rec.rows, rec.elapsed_ms)
            }
            QueryStatus::Failed => format!(
                "{} · 失败：{}",
                when_text,
                compact_text_preview(rec.error.as_deref().unwrap_or("未知错误"), ERROR_MAX_CHARS,)
            ),
        };
        if rec.sql_truncated {
            meta.push_str(" · 历史正文已截断");
        }
        if rec.error_truncated {
            meta.push_str(" · 错误详情已截断");
        }
        let sql_truncated = rec.sql_truncated;
        let rec_for_copy = rec.clone();
        let rec_for_fill = rec.clone();
        let rec_for_run = rec.clone();
        let rec_for_delete = rec;

        h_flex()
            .id(SharedString::from(format!("mhist-row-{ix}")))
            .items_center()
            .gap_3()
            .px_3()
            .py(px(8.0))
            .border_b_1()
            .border_color(border)
            .hover(move |s| s.bg(muted_bg))
            .child(
                div()
                    .w(px(8.0))
                    .h(px(8.0))
                    .rounded_full()
                    .bg(status_color)
                    .flex_none(),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(fg)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(preview),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted_fg)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(meta),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .child(
                        Button::new(SharedString::from(format!("mhist-copy-{ix}")))
                            .ghost()
                            .xsmall()
                            .label("复制")
                            .tooltip(if sql_truncated {
                                "历史正文不完整，已禁止复制"
                            } else {
                                "复制命令到剪贴板"
                            })
                            .disabled(sql_truncated)
                            .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    rec_for_copy.sql.clone(),
                                ));
                                window.push_notification(
                                    Notification::success("已复制命令").autohide(true),
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("mhist-fill-{ix}")))
                            .ghost()
                            .xsmall()
                            .label("填入编辑器")
                            .tooltip(if sql_truncated {
                                "历史正文不完整，无法安全填入编辑器"
                            } else {
                                "填入当前命令编辑器（不执行）"
                            })
                            .disabled(sql_truncated)
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                cx.emit(MongoHistoryEvent::FillEditor(rec_for_fill.sql.clone()));
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("mhist-run-{ix}")))
                            .ghost()
                            .xsmall()
                            .label("重跑")
                            .tooltip(if sql_truncated {
                                "历史正文不完整，无法安全重跑"
                            } else {
                                "填入编辑器并立即执行（失败记录亦可重试）"
                            })
                            .disabled(sql_truncated)
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                cx.emit(MongoHistoryEvent::RunCommand(rec_for_run.sql.clone()));
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("mhist-del-{ix}")))
                            .ghost()
                            .xsmall()
                            .label("删除")
                            .disabled(self.mutating)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.delete_record(rec_for_delete.id.clone(), cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}

impl Render for MongoHistoryList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let danger = theme.danger;
        let warning = theme.warning;
        let border = theme.border;

        let query = self.search.read(cx).value().trim().to_lowercase();
        let filtered: Vec<Arc<QueryRecord>> = self
            .records
            .iter()
            .filter(|record| record.matches_query_lower(&query))
            .cloned()
            .collect();

        let toolbar = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .pb(px(8.0))
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.search).small()),
            )
            .child(div().text_xs().text_color(muted_fg).child(format!(
                "{} / {}",
                filtered.len(),
                self.records.len()
            )))
            .when(self.history_truncated, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(warning)
                        .child("结果按 32 MiB 内存预算截断"),
                )
            })
            .when(self.mutating, |this| {
                this.child(div().text_xs().text_color(muted_fg).child("正在更新…"))
            })
            .child(
                Button::new("mhist-clear-all")
                    .ghost()
                    .xsmall()
                    .label("清空")
                    .tooltip("清空当前连接的全部查询历史")
                    .disabled(self.loading || self.mutating || self.records.is_empty())
                    .on_click(cx.listener(|_this, _: &ClickEvent, window, cx| {
                        let entity = cx.entity();
                        ramag_ui::open_confirm(
                            "清空查询历史？",
                            "将删除当前连接的全部历史记录，不可恢复。".to_string(),
                            "清空",
                            true,
                            move |_, app| {
                                entity.update(app, |this, cx| this.clear_all(cx));
                            },
                            window,
                            cx,
                        );
                    })),
            );

        let body: AnyElement = if self.loading {
            centered_hint("加载中…", muted_fg)
        } else if let Some(e) = &self.load_error {
            centered_hint(format!("加载失败：{e}"), danger)
        } else if self.records.is_empty() {
            centered_hint("暂无查询历史", muted_fg)
        } else if filtered.is_empty() {
            centered_hint(format!("没有匹配「{query}」的历史"), muted_fg)
        } else {
            let records = Rc::new(filtered);
            let rows = uniform_list(
                "mongo-history-rows",
                records.len(),
                cx.processor({
                    let records = records.clone();
                    move |this, range: Range<usize>, _window, cx| {
                        range
                            .map(|index| this.render_row(index, records[index].clone(), cx))
                            .collect::<Vec<_>>()
                    }
                }),
            )
            .size_full();
            div().size_full().child(rows).into_any_element()
        };

        v_flex()
            .w_full()
            .h(px(LIST_HEIGHT))
            .child(toolbar)
            .when_some(self.mutation_error.clone(), |this, error| {
                this.child(
                    div()
                        .w_full()
                        .py(px(6.0))
                        .text_xs()
                        .text_color(danger)
                        .child(error),
                )
            })
            .child(div().flex_1().min_h_0().child(body))
    }
}

fn centered_hint(text: impl Into<SharedString>, color: Hsla) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(color)
        .child(text.into())
        .into_any_element()
}
