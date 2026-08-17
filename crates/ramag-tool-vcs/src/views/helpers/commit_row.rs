//! History 列表单行渲染（IDEA Git 风格）

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};
use gpui_component::{
    ActiveTheme, h_flex,
    menu::{ContextMenuExt as _, PopupMenu},
};
use ramag_domain::entities::{Commit, ResetKind};

use super::super::commit_graph::{CommitGraphRow, lane_color, render_lane_gutter};
use super::super::vcs_view::VcsView;

const MAX_VISIBLE_REF_CHIPS: usize = 8;

/// History 单行：lane gutter + subject + refs + author + date + hash。左键打开详情，右键菜单
#[allow(clippy::too_many_arguments)]
pub(in crate::views) fn render_commit_row(
    c: &Commit,
    graph: &CommitGraphRow,
    mono: SharedString,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    accent: gpui::Hsla,
    selected: bool,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    let time_str = relative_time(&c.author.timestamp);
    let author_short = super::super::inline_text_preview(&c.author.name, 20);
    let dot_color = lane_color(usize::from(graph.lane));
    let hover_bg = cx.theme().muted;
    let mut sel_bg = accent;
    sel_bg.a = 0.12;

    let entity = cx.entity().clone();
    let cid = c.id.0.clone();

    let mut refs_row = h_flex().gap(px(4.0)).flex_none();
    for r in c.refs.iter().take(MAX_VISIBLE_REF_CHIPS) {
        refs_row = refs_row.child(ref_chip(r, accent));
    }
    if c.refs.len() > MAX_VISIBLE_REF_CHIPS {
        refs_row = refs_row.child(ref_chip(
            &format!("… +{}", c.refs.len() - MAX_VISIBLE_REF_CHIPS),
            accent,
        ));
    }

    let row_key: String = cid.chars().take(12).collect();
    let row_id = SharedString::from(format!("vcs-commit-row-{row_key}"));

    let cid_click = cid.clone();
    let on_click_handler = cx.listener(move |this, _: &ClickEvent, _, cx| {
        this.load_commit_detail(cid_click.clone(), cx);
    });

    let mut row = h_flex()
        .id(row_id)
        .w_full()
        .py(px(2.0))
        .items_center()
        .gap(px(0.0))
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg))
        .on_click(on_click_handler)
        .child(render_lane_gutter(graph))
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(px(6.0))
                .px(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(fg)
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(super::super::inline_text_preview(&c.subject, 240)),
                )
                .child(refs_row),
        )
        .child(
            div()
                .flex_none()
                .w(px(140.0))
                .px(px(6.0))
                .text_xs()
                .text_color(muted_fg)
                .overflow_hidden()
                .text_ellipsis()
                .child(author_short),
        )
        .child(
            div()
                .flex_none()
                .w(px(96.0))
                .px(px(6.0))
                .text_xs()
                .text_color(muted_fg)
                .child(time_str),
        )
        .child(
            div()
                .flex_none()
                .w(px(70.0))
                .px(px(6.0))
                .text_xs()
                .font_family(mono.clone())
                .text_color({
                    let mut col = dot_color;
                    col.a = 0.85;
                    col
                })
                .child(c.id.short().to_string()),
        );

    if selected {
        row = row.bg(sel_bg);
    }

    row.context_menu({
        let entity = entity.clone();
        let cid = cid.clone();
        move |menu: PopupMenu, _, _| {
            let (e1, c1) = (entity.clone(), cid.clone());
            let (e2, c2) = (entity.clone(), cid.clone());
            let (e3, c3) = (entity.clone(), cid.clone());
            let (e_sha, c_sha) = (entity.clone(), cid.clone());
            let (e_msg, c_msg) = (entity.clone(), cid.clone());
            menu.item(ramag_ui::menu_item("复制哈希").on_click(move |_, _, app| {
                app.write_to_clipboard(gpui::ClipboardItem::new_string(c_sha.clone()));
                e_sha.update(app, |this, cx| this.notify_success("已复制完整 SHA", cx));
            }))
            .item(ramag_ui::menu_item("复制说明").on_click(move |_, _, app| {
                e_msg.update(app, |this, cx| {
                    this.copy_commit_message(c_msg.clone(), cx);
                });
            }))
            .separator()
            .item(ramag_ui::menu_item("摘取").on_click(move |_, window, app| {
                use crate::views::confirm_dialogs::open_confirm_dialog;
                let short: String = c1.chars().take(7).collect();
                let c = c1.clone();
                open_confirm_dialog(
                    e1.clone(),
                    "摘取此提交？",
                    format!(
                        "将把「{short}」应用到当前分支；冲突时需处理后继续。"
                    ),
                    "摘取",
                    false,
                    move |this, cx| this.run_cherry_pick(c, cx),
                    window,
                    app,
                );
            }))
            .item(
                ramag_ui::menu_item("撤销提交").on_click(move |_, window, app| {
                    use crate::views::confirm_dialogs::open_confirm_dialog;
                    let short: String = c2.chars().take(7).collect();
                    let c = c2.clone();
                    open_confirm_dialog(
                        e2.clone(),
                        "撤销此提交？",
                        format!(
                            "将新建反向提交撤销「{short}」，不改写历史；冲突时需处理后继续。"
                        ),
                        "撤销",
                        false,
                        move |this, cx| this.run_revert(c, cx),
                        window,
                        app,
                    );
                }),
            )
            .item(
                ramag_ui::menu_item("混合重置").on_click(move |_, window, app| {
                    use crate::views::confirm_dialogs::open_confirm_dialog;
                    let short: String = c3.chars().take(7).collect();
                    let c = c3.clone();
                    open_confirm_dialog(
                        e3.clone(),
                        "混合重置？",
                        format!(
                            "HEAD 将移至「{short}」；之后提交可从 reflog 找回，改动回到未暂存状态，工作区文件保留。"
                        ),
                        "重置",
                        false,
                        move |this, cx| this.run_reset(c, ResetKind::Mixed, cx),
                        window,
                        app,
                    );
                }),
            )
        }
    })
    .into_any_element()
}

/// 把 chrono::DateTime 渲染成「3 天前 / 2 小时前 / 刚刚」相对时间
fn relative_time(ts: &chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let secs = (now - *ts).num_seconds();
    if secs < 60 {
        return "刚刚".into();
    }
    if secs < 3600 {
        return format!("{} 分钟前", secs / 60);
    }
    if secs < 86400 {
        return format!("{} 小时前", secs / 3600);
    }
    if secs < 86400 * 30 {
        return format!("{} 天前", secs / 86400);
    }
    if secs < 86400 * 365 {
        return format!("{} 个月前", secs / (86400 * 30));
    }
    ts.format("%Y-%m-%d").to_string()
}

/// commit refs 标签：根据 ref 名前缀决定颜色（HEAD / origin/* / tag: *）
fn ref_chip(name: &str, accent: gpui::Hsla) -> AnyElement {
    // tag 名习惯以 "tag: " 前缀（git log --decorate）
    let (label, tone) = if let Some(rest) = name.strip_prefix("tag: ") {
        (
            super::super::inline_text_preview(rest, 80),
            gpui::hsla(40.0 / 360.0, 0.7, 0.55, 1.0),
        )
    } else if name.starts_with("HEAD") {
        (
            super::super::inline_text_preview(name, 80),
            gpui::hsla(140.0 / 360.0, 0.55, 0.45, 1.0),
        )
    } else if name.contains('/') {
        // remote-tracking：origin/main 等
        (
            super::super::inline_text_preview(name, 80),
            gpui::hsla(220.0 / 360.0, 0.6, 0.55, 1.0),
        )
    } else {
        (super::super::inline_text_preview(name, 80), accent)
    };
    let mut bg = tone;
    bg.a = 0.16;
    div()
        .px(px(6.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .bg(bg)
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(tone)
        .child(label)
        .into_any_element()
}
