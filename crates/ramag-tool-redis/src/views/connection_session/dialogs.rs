//! Redis Session 9 个 open_*_dialog + 破坏性操作二次确认

use std::rc::Rc;

use gpui::{App, AppContext as _, Context, ParentElement, SharedString, Styled, Window, px};
use gpui_component::{WindowExt as _, notification::Notification};
use tracing::info;

use super::RedisSessionPanel;
use crate::views::hash_field_form::{HashFieldForm, HashFieldFormEvent, HashFieldFormMode};
use crate::views::inline_text_preview;
use crate::views::key_create::{KeyCreateEvent, KeyCreateForm};
use crate::views::list_element_form::{ListElementForm, ListElementFormEvent};
use crate::views::set_element_form::{SetElementForm, SetElementFormEvent};
use crate::views::stream_entry_form::{StreamEntryForm, StreamEntryFormEvent};
use crate::views::ttl_edit::{TtlEditEvent, TtlEditForm};
use crate::views::value_edit::{ValueEditEvent, ValueEditForm};
use crate::views::zset_element_form::{ZSetElementForm, ZSetElementFormEvent, ZSetElementFormMode};

/// 二次确认弹窗的回调签名（避免 `Rc<dyn Fn(...)>` 长类型重复出现）
pub(super) type ConfirmCallback = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

impl RedisSessionPanel {
    /// 弹窗保存后：仅当主区当前 key 与弹窗目标 key 一致时才刷新；
    /// 用户在弹窗期间切了别的 key 则跳过（已经看不到了）
    pub(super) fn reload_detail_if_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let matches = self
            .detail
            .read(cx)
            .current_key()
            .map(|k| k == key)
            .unwrap_or(false);
        if matches {
            self.detail.update(cx, |p, cx| p.reload_current(cx));
        }
    }

    pub(super) fn open_create_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let svc = self.service.clone();
        let config = self.config.clone();
        let db = self.db;
        let form = cx.new(|cx| KeyCreateForm::new(svc, config, db, window, cx));
        let tree_for_refresh = self.tree.clone();
        let sub = cx.subscribe_in(
            &form,
            window,
            move |this: &mut Self, _, ev: &KeyCreateEvent, window, cx| {
                this.clear_dialog_subscription();
                match ev {
                    KeyCreateEvent::Created { key, ttl_warning } => {
                        info!(key_bytes = key.len(), "key created via dialog");
                        let new_key = key.clone();
                        window.close_dialog(cx);
                        if let Some(warning) = ttl_warning {
                            window.push_notification(
                                Notification::warning(warning.clone())
                                    .title("Key 已创建，但 TTL 未按预期设置"),
                                cx,
                            );
                        }
                        tree_for_refresh.update(cx, |t, cx| {
                            t.refresh(cx);
                            t.select_key_external(new_key.clone(), cx);
                        });
                    }
                    KeyCreateEvent::Cancelled => window.close_dialog(cx),
                }
            },
        );
        self.set_dialog_subscription(sub);
        let form_for_dialog = form.clone();
        let session_for_close = cx.entity().clone();
        window.open_dialog(cx, move |dialog, _w, _app| {
            let form = form_for_dialog.clone();
            let form_for_cancel = form_for_dialog.clone();
            let session_for_close = session_for_close.clone();
            dialog
                .title("新建 Key")
                .close_button(false)
                .on_cancel(move |_, _, app| !form_for_cancel.read(app).is_submitting())
                .on_close(move |_, _, app| {
                    session_for_close.update(app, |this, _| this.clear_dialog_subscription());
                })
                .w(px(640.0))
                .p(px(24.0))
                .content(move |content, _, _| content.child(form.clone()))
        });
    }

    pub(super) fn open_ttl_dialog(
        &mut self,
        key: String,
        ttl_ms: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let svc = self.service.clone();
        let config = self.config.clone();
        let db = self.db;
        let key_for_form = key.clone();
        let form = cx.new(|cx| TtlEditForm::new(svc, config, db, key_for_form, ttl_ms, window, cx));
        let key_for_reload = key.clone();
        let sub = cx.subscribe_in(
            &form,
            window,
            move |this: &mut Self, _, ev: &TtlEditEvent, window, cx| {
                this.clear_dialog_subscription();
                match ev {
                    TtlEditEvent::Updated(_) => {
                        info!(key_bytes = key_for_reload.len(), "ttl updated");
                        window.close_dialog(cx);
                        this.reload_detail_if_key(&key_for_reload, cx);
                    }
                    TtlEditEvent::Cancelled => window.close_dialog(cx),
                }
            },
        );
        self.set_dialog_subscription(sub);
        let form_for_dialog = form.clone();
        let session_for_close = cx.entity().clone();
        window.open_dialog(cx, move |dialog, _w, _app| {
            let form = form_for_dialog.clone();
            let form_for_cancel = form_for_dialog.clone();
            let session_for_close = session_for_close.clone();
            dialog
                .title(format!("编辑 TTL · {}", inline_text_preview(&key, 96)))
                .close_button(false)
                .on_cancel(move |_, _, app| !form_for_cancel.read(app).is_submitting())
                .on_close(move |_, _, app| {
                    session_for_close.update(app, |this, _| this.clear_dialog_subscription());
                })
                .w(px(520.0))
                .p(px(24.0))
                .content(move |content, _, _| content.child(form.clone()))
        });
    }

    pub(super) fn open_value_dialog(
        &mut self,
        key: String,
        current_value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let svc = self.service.clone();
        let config = self.config.clone();
        let db = self.db;
        let key_for_form = key.clone();
        let form = cx
            .new(|cx| ValueEditForm::new(svc, config, db, key_for_form, current_value, window, cx));
        let key_for_reload = key.clone();
        let sub = cx.subscribe_in(
            &form,
            window,
            move |this: &mut Self, _, ev: &ValueEditEvent, window, cx| {
                this.clear_dialog_subscription();
                match ev {
                    ValueEditEvent::Saved => {
                        info!(key_bytes = key_for_reload.len(), "value saved");
                        window.close_dialog(cx);
                        this.reload_detail_if_key(&key_for_reload, cx);
                    }
                    ValueEditEvent::Cancelled => window.close_dialog(cx),
                }
            },
        );
        self.set_dialog_subscription(sub);
        let form_for_dialog = form.clone();
        let session_for_close = cx.entity().clone();
        window.open_dialog(cx, move |dialog, _w, _app| {
            let form = form_for_dialog.clone();
            let form_for_cancel = form_for_dialog.clone();
            let session_for_close = session_for_close.clone();
            dialog
                .title(format!("编辑值 · {}", inline_text_preview(&key, 96)))
                .close_button(false)
                .on_cancel(move |_, _, app| !form_for_cancel.read(app).is_submitting())
                .on_close(move |_, _, app| {
                    session_for_close.update(app, |this, _| this.clear_dialog_subscription());
                })
                .w(px(640.0))
                .p(px(24.0))
                .content(move |content, _, _| content.child(form.clone()))
        });
    }

    pub(super) fn open_hash_field_dialog(
        &mut self,
        key: String,
        mode: HashFieldFormMode,
        initial_value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let svc = self.service.clone();
        let config = self.config.clone();
        let db = self.db;
        let key_for_form = key.clone();
        let mode_for_form = mode.clone();
        let form = cx.new(|cx| {
            HashFieldForm::new(
                svc,
                config,
                db,
                key_for_form,
                mode_for_form,
                initial_value,
                window,
                cx,
            )
        });
        let key_for_reload = key.clone();
        let sub = cx.subscribe_in(
            &form,
            window,
            move |this: &mut Self, _, ev: &HashFieldFormEvent, window, cx| {
                this.clear_dialog_subscription();
                match ev {
                    HashFieldFormEvent::Saved { field } => {
                        info!(field_bytes = field.len(), "hash field saved");
                        window.close_dialog(cx);
                        this.reload_detail_if_key(&key_for_reload, cx);
                    }
                    HashFieldFormEvent::Cancelled => window.close_dialog(cx),
                }
            },
        );
        self.set_dialog_subscription(sub);
        let key_preview = inline_text_preview(&key, 64);
        let title = match &mode {
            HashFieldFormMode::Add => format!("新增字段 · {key_preview}"),
            HashFieldFormMode::Edit { field } => format!(
                "编辑字段 · {key_preview} · {}",
                inline_text_preview(field, 64)
            ),
        };
        let form_for_dialog = form.clone();
        let session_for_close = cx.entity().clone();
        window.open_dialog(cx, move |dialog, _w, _app| {
            let form = form_for_dialog.clone();
            let form_for_cancel = form_for_dialog.clone();
            let session_for_close = session_for_close.clone();
            let title = title.clone();
            dialog
                .title(title)
                .close_button(false)
                .on_cancel(move |_, _, app| !form_for_cancel.read(app).is_submitting())
                .on_close(move |_, _, app| {
                    session_for_close.update(app, |this, _| this.clear_dialog_subscription());
                })
                .w(px(640.0))
                .p(px(24.0))
                .content(move |content, _, _| content.child(form.clone()))
        });
    }

    pub(super) fn open_list_element_dialog(
        &mut self,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let svc = self.service.clone();
        let config = self.config.clone();
        let db = self.db;
        let form = cx.new(|cx| ListElementForm::new(svc, config, db, key.clone(), window, cx));
        let key_for_reload = key.clone();
        let sub = cx.subscribe_in(
            &form,
            window,
            move |this: &mut Self, _, ev: &ListElementFormEvent, window, cx| {
                this.clear_dialog_subscription();
                match ev {
                    ListElementFormEvent::Saved => {
                        window.close_dialog(cx);
                        this.reload_detail_if_key(&key_for_reload, cx);
                    }
                    ListElementFormEvent::Cancelled => window.close_dialog(cx),
                }
            },
        );
        self.set_dialog_subscription(sub);
        let form_for_dialog = form.clone();
        let session_for_close = cx.entity().clone();
        window.open_dialog(cx, move |dialog, _w, _app| {
            let form = form_for_dialog.clone();
            let form_for_cancel = form_for_dialog.clone();
            let session_for_close = session_for_close.clone();
            let title = format!("新增 List 元素 · {}", inline_text_preview(&key, 96));
            dialog
                .title(title)
                .close_button(false)
                .on_cancel(move |_, _, app| !form_for_cancel.read(app).is_submitting())
                .on_close(move |_, _, app| {
                    session_for_close.update(app, |this, _| this.clear_dialog_subscription());
                })
                .w(px(640.0))
                .p(px(24.0))
                .content(move |content, _, _| content.child(form.clone()))
        });
    }

    pub(super) fn open_set_element_dialog(
        &mut self,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let svc = self.service.clone();
        let config = self.config.clone();
        let db = self.db;
        let form = cx.new(|cx| SetElementForm::new(svc, config, db, key.clone(), window, cx));
        let key_for_reload = key.clone();
        let sub = cx.subscribe_in(
            &form,
            window,
            move |this: &mut Self, _, ev: &SetElementFormEvent, window, cx| {
                this.clear_dialog_subscription();
                match ev {
                    SetElementFormEvent::Saved => {
                        window.close_dialog(cx);
                        this.reload_detail_if_key(&key_for_reload, cx);
                    }
                    SetElementFormEvent::Cancelled => window.close_dialog(cx),
                }
            },
        );
        self.set_dialog_subscription(sub);
        let form_for_dialog = form.clone();
        let session_for_close = cx.entity().clone();
        window.open_dialog(cx, move |dialog, _w, _app| {
            let form = form_for_dialog.clone();
            let form_for_cancel = form_for_dialog.clone();
            let session_for_close = session_for_close.clone();
            let title = format!("新增 Set 元素 · {}", inline_text_preview(&key, 96));
            dialog
                .title(title)
                .close_button(false)
                .on_cancel(move |_, _, app| !form_for_cancel.read(app).is_submitting())
                .on_close(move |_, _, app| {
                    session_for_close.update(app, |this, _| this.clear_dialog_subscription());
                })
                .w(px(640.0))
                .p(px(24.0))
                .content(move |content, _, _| content.child(form.clone()))
        });
    }

    pub(super) fn open_zset_element_dialog(
        &mut self,
        key: String,
        mode: ZSetElementFormMode,
        initial_score: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let svc = self.service.clone();
        let config = self.config.clone();
        let db = self.db;
        let key_for_form = key.clone();
        let mode_for_form = mode.clone();
        let form = cx.new(|cx| {
            ZSetElementForm::new(
                svc,
                config,
                db,
                key_for_form,
                mode_for_form,
                initial_score,
                window,
                cx,
            )
        });
        let key_for_reload = key.clone();
        let sub = cx.subscribe_in(
            &form,
            window,
            move |this: &mut Self, _, ev: &ZSetElementFormEvent, window, cx| {
                this.clear_dialog_subscription();
                match ev {
                    ZSetElementFormEvent::Saved => {
                        window.close_dialog(cx);
                        this.reload_detail_if_key(&key_for_reload, cx);
                    }
                    ZSetElementFormEvent::Cancelled => window.close_dialog(cx),
                }
            },
        );
        self.set_dialog_subscription(sub);
        let key_preview = inline_text_preview(&key, 64);
        let title = match &mode {
            ZSetElementFormMode::Add => format!("新增 ZSet 成员 · {key_preview}"),
            ZSetElementFormMode::EditScore { member } => format!(
                "改 Score · {key_preview} · {}",
                inline_text_preview(member, 64)
            ),
        };
        let form_for_dialog = form.clone();
        let session_for_close = cx.entity().clone();
        window.open_dialog(cx, move |dialog, _w, _app| {
            let form = form_for_dialog.clone();
            let form_for_cancel = form_for_dialog.clone();
            let session_for_close = session_for_close.clone();
            let title = title.clone();
            dialog
                .title(title)
                .close_button(false)
                .on_cancel(move |_, _, app| !form_for_cancel.read(app).is_submitting())
                .on_close(move |_, _, app| {
                    session_for_close.update(app, |this, _| this.clear_dialog_subscription());
                })
                .w(px(560.0))
                .p(px(24.0))
                .content(move |content, _, _| content.child(form.clone()))
        });
    }

    pub(super) fn open_stream_entry_dialog(
        &mut self,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let svc = self.service.clone();
        let config = self.config.clone();
        let db = self.db;
        let form = cx.new(|cx| StreamEntryForm::new(svc, config, db, key.clone(), window, cx));
        let key_for_reload = key.clone();
        let sub = cx.subscribe_in(
            &form,
            window,
            move |this: &mut Self, _, ev: &StreamEntryFormEvent, window, cx| {
                this.clear_dialog_subscription();
                match ev {
                    StreamEntryFormEvent::Saved => {
                        window.close_dialog(cx);
                        this.reload_detail_if_key(&key_for_reload, cx);
                    }
                    StreamEntryFormEvent::Cancelled => window.close_dialog(cx),
                }
            },
        );
        self.set_dialog_subscription(sub);
        let form_for_dialog = form.clone();
        let session_for_close = cx.entity().clone();
        window.open_dialog(cx, move |dialog, _w, _app| {
            let form = form_for_dialog.clone();
            let form_for_cancel = form_for_dialog.clone();
            let session_for_close = session_for_close.clone();
            let title = format!("新增 Stream 条目 · {}", inline_text_preview(&key, 96));
            dialog
                .title(title)
                .close_button(false)
                .on_cancel(move |_, _, app| !form_for_cancel.read(app).is_submitting())
                .on_close(move |_, _, app| {
                    session_for_close.update(app, |this, _| this.clear_dialog_subscription());
                })
                .w(px(640.0))
                .p(px(24.0))
                .content(move |content, _, _| content.child(form.clone()))
        });
    }

    /// 委托 `ramag_ui::open_confirm`，danger=true、按钮文案固定「删除」
    pub(super) fn confirm_delete_op(
        &mut self,
        title: SharedString,
        desc: String,
        on_confirm: ConfirmCallback,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = self;
        ramag_ui::open_confirm(
            title,
            desc,
            "删除",
            true,
            move |window, app| on_confirm(window, app),
            window,
            cx,
        );
    }
}

/// 截断弹窗中要展示的字符串到指定字符数（按 char 计，避免破坏 utf-8 边界）
/// 超长加省略号，便于在「删除 X」对话框里清晰展示目标
pub(super) fn truncate_for_dialog(s: &str, max_chars: usize) -> String {
    inline_text_preview(s, max_chars)
}

#[cfg(test)]
mod tests {
    use super::truncate_for_dialog;

    #[test]
    fn short_string_unchanged() {
        assert_eq!(truncate_for_dialog("abc", 10), "abc");
    }

    #[test]
    fn exact_length_unchanged() {
        assert_eq!(truncate_for_dialog("abcde", 5), "abcde");
    }

    #[test]
    fn over_length_truncated() {
        assert_eq!(truncate_for_dialog("abcdef", 3), "abc…");
    }

    #[test]
    fn utf8_chinese_safe() {
        // 5 个汉字 = 5 chars，截到 3 → 前 3 个汉字 + 省略号
        assert_eq!(truncate_for_dialog("你好世界呀", 3), "你好世…");
    }

    #[test]
    fn utf8_emoji_safe() {
        // emoji 单独占 1 char（只算 unicode codepoint，足够避免 utf-8 边界 panic）
        let r = truncate_for_dialog("ab😀cd", 3);
        assert_eq!(r, "ab😀…");
    }

    #[test]
    fn empty_string() {
        assert_eq!(truncate_for_dialog("", 5), "");
    }

    #[test]
    fn control_characters_are_safe_for_single_line_dialogs() {
        assert_eq!(truncate_for_dialog("a\nb\0c", 10), "a b�c");
    }
}
