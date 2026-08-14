use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    App, AppContext as _, ClickEvent, Context, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, WindowExt as _, button::ButtonVariants as _,
    h_flex, v_flex,
};
use ramag_domain::entities::ConflictPolicy;

type ImportPickHandler = Box<dyn FnOnce(ConflictPolicy, Vec<PathBuf>, &mut Window, &mut App)>;

/// 导入选项表单。
struct ImportOptionsForm {
    description: SharedString,
    offer_merge: bool,
    filter_label: &'static str,
    extensions: &'static [&'static str],
    policy: ConflictPolicy,
    files: Vec<PathBuf>,
    /// 防止文件框重复打开。
    picking: bool,
    on_pick: Rc<RefCell<Option<ImportPickHandler>>>,
}

impl ImportOptionsForm {
    /// 打开系统多选文件框。
    fn pick_files(&mut self, cx: &mut Context<Self>) {
        if self.picking {
            return;
        }
        self.picking = true;
        cx.notify();
        let filter_label = self.filter_label;
        let extensions = self.extensions;
        cx.spawn(async move |this, cx| {
            let picked = rfd::AsyncFileDialog::new()
                .add_filter(filter_label, extensions)
                .pick_files()
                .await;
            let _ = this.update(cx, |this, cx| {
                this.picking = false;
                if let Some(handles) = picked
                    && !handles.is_empty()
                {
                    this.files = handles
                        .iter()
                        .map(|handle| handle.path().to_path_buf())
                        .collect();
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn files_summary(&self) -> String {
        fn name_of(path: &std::path::Path) -> String {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        }
        match self.files.as_slice() {
            [] => "未选择文件".to_string(),
            [only] => name_of(only),
            [first, second] => format!("{}、{}", name_of(first), name_of(second)),
            [first, ..] => format!("{} 等 {} 个文件", name_of(first), self.files.len()),
        }
    }
}

impl Render for ImportOptionsForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted_fg = cx.theme().muted_foreground;
        let entity = cx.entity();

        let policy_button = {
            let entity = entity.clone();
            move |id: &'static str,
                  label: &'static str,
                  hint: &'static str,
                  value: ConflictPolicy,
                  danger: bool,
                  selected: bool| {
                let entity = entity.clone();
                let mut button = crate::clickable_button(id)
                    .small()
                    .flex_1()
                    .label(label)
                    .tooltip(hint);
                button = match (selected, danger) {
                    (true, true) => button.danger(),
                    (true, false) => button.primary(),
                    (false, _) => button.outline(),
                };
                button.on_click(move |_: &ClickEvent, _, app| {
                    entity.update(app, |this, cx| {
                        this.policy = value;
                        cx.notify();
                    });
                })
            }
        };
        let mut policy_row = h_flex().w_full().gap(px(8.0)).child(policy_button(
            "ramag-import-skip",
            "跳过",
            "跳过同名对象（推荐）",
            ConflictPolicy::Skip,
            false,
            self.policy == ConflictPolicy::Skip,
        ));
        if self.offer_merge {
            policy_row = policy_row.child(policy_button(
                "ramag-import-merge",
                "合并",
                "保留对象，补齐缺失条目",
                ConflictPolicy::Merge,
                false,
                self.policy == ConflictPolicy::Merge,
            ));
        }
        policy_row = policy_row
            .child(policy_button(
                "ramag-import-overwrite",
                "覆盖",
                "删除同名对象后导入，不可恢复",
                ConflictPolicy::Overwrite,
                true,
                self.policy == ConflictPolicy::Overwrite,
            ))
            .child(policy_button(
                "ramag-import-fail",
                "停止",
                "遇到同名对象即停止",
                ConflictPolicy::Fail,
                false,
                self.policy == ConflictPolicy::Fail,
            ));

        let pick_button = {
            let entity = entity.clone();
            crate::clickable_button("ramag-import-pick")
                .outline()
                .small()
                .label(if self.files.is_empty() {
                    "选择文件"
                } else {
                    "重选"
                })
                .disabled(self.picking)
                .on_click(move |_: &ClickEvent, _, app| {
                    entity.update(app, |this, cx| this.pick_files(cx));
                })
        };
        let confirm_button = {
            let entity = entity.clone();
            crate::clickable_button("ramag-import-confirm")
                .primary()
                .small()
                .label("导入")
                .disabled(self.files.is_empty() || self.picking)
                .on_click(move |_: &ClickEvent, window, app| {
                    let taken = entity.update(app, |this, _| {
                        if this.files.is_empty() {
                            return None;
                        }
                        this.on_pick
                            .borrow_mut()
                            .take()
                            .map(|handler| (handler, this.policy, std::mem::take(&mut this.files)))
                    });
                    if let Some((handler, policy, files)) = taken {
                        window.close_dialog(app);
                        handler(policy, files, window, app);
                    }
                })
        };
        let cancel_button = crate::clickable_button("ramag-import-cancel")
            .ghost()
            .small()
            .label("取消")
            .on_click(|_: &ClickEvent, window, app| window.close_dialog(app));

        v_flex()
            .w(px(560.0))
            .gap(px(10.0))
            .child(
                div()
                    .py(px(2.0))
                    .text_sm()
                    .text_color(muted_fg)
                    .child(self.description.clone()),
            )
            .child(
                v_flex()
                    .gap(px(6.0))
                    .child(div().text_xs().text_color(muted_fg).child("同名对象"))
                    .child(policy_row),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(pick_button)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(muted_fg)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(self.files_summary()),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_end()
                    .gap(px(8.0))
                    .child(cancel_button)
                    .child(confirm_button),
            )
    }
}

/// 打开导入选项对话框。
pub fn open_import_options_dialog(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    offer_merge: bool,
    file_filter: (&'static str, &'static [&'static str]),
    on_pick: impl FnOnce(ConflictPolicy, Vec<PathBuf>, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let title: SharedString = title.into();
    let description: SharedString = description.into();
    let (filter_label, extensions) = file_filter;
    let form = cx.new(|_| ImportOptionsForm {
        description,
        offer_merge,
        filter_label,
        extensions,
        policy: ConflictPolicy::Skip,
        files: Vec::new(),
        picking: false,
        on_pick: Rc::new(RefCell::new(Some(Box::new(on_pick)))),
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let form = form.clone();
        dialog
            .title(crate::closable_dialog_title(
                "ramag-import-close",
                title.clone(),
                |_, _| {},
            ))
            .close_button(false)
            .margin_top(px(160.0))
            .content(move |content, _, _| content.child(form.clone()))
    });
}
