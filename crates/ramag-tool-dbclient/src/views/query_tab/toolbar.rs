use gpui::{ClickEvent, Context, IntoElement, ParentElement, Styled, div, px};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, button::ButtonVariants as _,
    h_flex,
};

use super::QueryTab;

pub(super) fn render_delete_button(
    plan_visible: bool,
    has_selected: bool,
    modify_reason: Option<&'static str>,
    cx: &mut Context<QueryTab>,
) -> impl IntoElement {
    let delete_tip: gpui::SharedString = match (modify_reason, has_selected) {
        (Some(reason), _) => reason.into(),
        (None, false) => "请先选择数据".into(),
        (None, true) => "删除选中行".into(),
    };
    ramag_ui::clickable_button("toolbar-delete")
        .ghost()
        .small()
        .icon(gpui_component::IconName::Minus)
        .tooltip(delete_tip)
        .disabled(plan_visible || !has_selected || modify_reason.is_some())
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            let panel_ref = this.active_result().read(cx);
            let multi = panel_ref.delete_preview_multi(cx);
            let single = if multi.is_none() {
                panel_ref.delete_preview(cx)
            } else {
                None
            };
            let _ = panel_ref;
            if let Some((indices, _)) = &multi
                && !this.active_result().update(cx, |panel, cx| {
                    panel.guard_batch_delete_count(indices.len(), cx)
                })
            {
                return;
            }
            let result = this.active_result();
            let (title, preview, on_ok_indices, on_ok_single): (
                &'static str,
                String,
                Option<Vec<usize>>,
                Option<usize>,
            ) = match (multi, single) {
                (Some((ids, summary)), _) => ("删除选中行？", summary, Some(ids), None),
                (None, Some((ri, p))) => ("删除此行？", format!("将删除：{p}"), None, Some(ri)),
                _ => return,
            };
            window.open_dialog(cx, move |dialog, _, _| {
                let result_btn = result.clone();
                let preview_for_content = preview.clone();
                let on_ok_indices = on_ok_indices.clone();
                let on_ok_single = on_ok_single;
                let cancel = ramag_ui::clickable_button("del-row-cancel")
                    .ghost()
                    .small()
                    .label("取消")
                    .on_click(|_: &ClickEvent, window, app| {
                        window.close_dialog(app);
                    });
                let ok = ramag_ui::clickable_button("del-row-ok")
                    .danger()
                    .small()
                    .label("删除")
                    .on_click({
                        let result = result_btn.clone();
                        let indices = on_ok_indices.clone();
                        let single = on_ok_single;
                        move |_: &ClickEvent, window, app| {
                            let started = result.update(app, |r, cx| {
                                if let Some(ids) = indices.clone() {
                                    r.execute_delete_rows_async(ids, cx)
                                } else if let Some(ri) = single {
                                    r.execute_delete_row_async(ri, cx)
                                } else {
                                    false
                                }
                            });
                            if started {
                                window.close_dialog(app);
                            }
                        }
                    });
                dialog
                    .title(ramag_ui::closable_dialog_title(
                        "delete-row-close",
                        title,
                        |_, _| {},
                    ))
                    .close_button(false)
                    .width(px(520.0))
                    .margin_top(px(180.0))
                    .content(move |c, _, cx| {
                        let muted_fg = cx.theme().muted_foreground;
                        let p = preview_for_content.clone();
                        c.child(div().text_sm().text_color(muted_fg).child(p))
                    })
                    .footer(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_end()
                            .gap(px(8.0))
                            .child(cancel)
                            .child(ok),
                    )
            });
        }))
}
