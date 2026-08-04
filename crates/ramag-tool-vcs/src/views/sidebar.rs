//! 折叠段共享件：SidebarSection + section_header + history 左栏行类型 / 分发。
//! 左栏（本地/远程分支 + Tag）合并为单个 uniform_list，所有行统一 28px 等高

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement, SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _,
    h_flex,
    input::Input,
    menu::{ContextMenuExt as _, PopupMenu},
    v_flex,
};

use super::vcs_view::VcsView;

/// 行高固定 28px：uniform_list 行级虚拟化要求所有行等高
pub(super) const LEFT_ROW_H: f32 = 28.0;

/// 折叠段标识（用于 section_header 点击切换状态）
#[derive(Debug, Clone, Copy)]
pub(super) enum SidebarSection {
    Local,
    Remote,
    Tag,
    /// 远程仓库配置（origin 等），区别于 `Remote`（远程分支引用）
    RemoteRepo,
}

/// 左栏扁平行：段表头 / 分支 / Tag / 空占位。
pub(super) enum LeftRow {
    Header {
        title: &'static str,
        count: usize,
        collapsed: bool,
        section: SidebarSection,
    },
    Branch {
        idx: usize,
        is_remote: bool,
    },
    Tag {
        idx: usize,
    },
    Remote {
        idx: usize,
    },
    Empty(&'static str),
}

impl VcsView {
    /// uniform_list 单行分发（左栏：分支段 + Tag 段）
    pub(super) fn render_left_row(&self, row: &LeftRow, cx: &mut Context<Self>) -> AnyElement {
        match row {
            LeftRow::Header {
                title,
                count,
                collapsed,
                section,
            } => section_header(title, *count, *collapsed, *section, self.busy, cx),
            LeftRow::Branch { idx, is_remote } => {
                let branch = if *is_remote {
                    self.remote_branches.get(*idx)
                } else {
                    self.local_branches.get(*idx)
                };
                branch.map_or_else(
                    || div().h(px(LEFT_ROW_H)).into_any_element(),
                    |branch| {
                        super::sidebar_branches::branch_row(*idx, branch, self.busy, *is_remote, cx)
                            .into_any_element()
                    },
                )
            }
            LeftRow::Tag { idx } => self.tags.get(*idx).map_or_else(
                || div().h(px(LEFT_ROW_H)).into_any_element(),
                |tag| super::sidebar_tags::tag_row(*idx, tag, self.busy, cx).into_any_element(),
            ),
            LeftRow::Remote { idx } => self.remotes.get(*idx).map_or_else(
                || div().h(px(LEFT_ROW_H)).into_any_element(),
                |remote| {
                    super::sidebar_remotes::remote_row(*idx, remote, self.busy, cx)
                        .into_any_element()
                },
            ),
            LeftRow::Empty(msg) => {
                let muted_fg = cx.theme().muted_foreground;
                h_flex()
                    .h(px(LEFT_ROW_H))
                    .flex_none()
                    .items_center()
                    .pl(px(4.0))
                    .text_xs()
                    .text_color(muted_fg)
                    .child(*msg)
                    .into_any_element()
            }
        }
    }
}

/// 段标题：折叠图标 + 名称 + 计数，整行可点折叠（固定 28px 高）
pub(super) fn section_header(
    title: &'static str,
    count: usize,
    collapsed: bool,
    sec: SidebarSection,
    busy: bool,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    let theme = cx.theme();
    let muted_fg = theme.muted_foreground;
    let chev = if collapsed {
        IconName::ChevronRight
    } else {
        IconName::ChevronDown
    };
    let id = SharedString::from(format!(
        "vcs-side-section-{}",
        match sec {
            SidebarSection::Local => "local",
            SidebarSection::Remote => "remote",
            SidebarSection::Tag => "tag",
            SidebarSection::RemoteRepo => "remote-repo",
        }
    ));
    let hover_bg = theme.muted;

    let can_create = !matches!(sec, SidebarSection::Remote);
    let entity = cx.entity();

    let row = h_flex()
        .id(id)
        .h(px(LEFT_ROW_H))
        .flex_none()
        .gap(px(4.0))
        .items_center()
        .px(px(2.0))
        .rounded(px(3.0))
        .cursor_pointer()
        .hover(move |this| this.bg(hover_bg))
        .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
            match sec {
                SidebarSection::Local => this.collapsed_local = !this.collapsed_local,
                SidebarSection::Remote => this.collapsed_remote = !this.collapsed_remote,
                SidebarSection::Tag => this.collapsed_tag = !this.collapsed_tag,
                SidebarSection::RemoteRepo => {
                    this.collapsed_remote_repos = !this.collapsed_remote_repos
                }
            }
            cx.notify();
        }))
        .child(Icon::new(chev).xsmall().text_color(muted_fg))
        .child(
            div()
                .flex_1()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(muted_fg)
                .child(format!("{title} ({count})")),
        );
    if can_create {
        row.context_menu(move |menu: PopupMenu, _, _| {
            let entity = entity.clone();
            menu.item(
                ramag_ui::menu_item_with_disabled(section_create_label(sec), busy).on_click(
                    move |_: &ClickEvent, window, app| {
                        open_sidebar_create_dialog(entity.clone(), sec, window, app);
                    },
                ),
            )
        })
        .into_any_element()
    } else {
        row.into_any_element()
    }
}

fn section_create_label(section: SidebarSection) -> &'static str {
    match section {
        SidebarSection::Local => "新建分支",
        SidebarSection::Tag => "新建 Tag",
        SidebarSection::RemoteRepo => "添加远程仓库",
        SidebarSection::Remote => "",
    }
}

pub(super) fn open_sidebar_create_dialog(
    view: Entity<VcsView>,
    section: SidebarSection,
    window: &mut Window,
    app: &mut App,
) {
    if view.read(app).busy {
        return;
    }
    match section {
        SidebarSection::Local => open_create_branch_dialog(view, window, app),
        SidebarSection::Tag => open_create_tag_dialog(view, window, app),
        SidebarSection::RemoteRepo => open_create_remote_dialog(view, window, app),
        SidebarSection::Remote => {}
    }
}

fn open_create_branch_dialog(view: Entity<VcsView>, window: &mut Window, app: &mut App) {
    let dialog_data = {
        let this = view.read(app);
        this.status
            .as_ref()
            .and_then(|status| status.head_commit.as_ref())
            .map(|_| {
                let head = this
                    .status
                    .as_ref()
                    .and_then(|status| status.head_branch.clone())
                    .unwrap_or_else(|| "(HEAD)".into());
                let local = this
                    .local_branches
                    .iter()
                    .map(|branch| (branch.name.clone(), branch.is_head))
                    .collect();
                let remote = this
                    .remote_branches
                    .iter()
                    .map(|branch| branch.name.clone())
                    .collect();
                (head, local, remote)
            })
    };
    let Some((head, local, remote)) = dialog_data else {
        view.update(app, |this, cx| {
            this.error = Some("请先创建首个提交，再新建分支".into());
            cx.notify();
        });
        return;
    };
    super::branch_picker::open_new_branch_dialog(view, head, local, remote, window, app);
}

fn open_create_tag_dialog(view: Entity<VcsView>, window: &mut Window, app: &mut App) {
    let (name, message) = {
        let this = view.read(app);
        (
            this.create_tag_input.clone(),
            this.create_tag_message_input.clone(),
        )
    };
    for input in [&name, &message] {
        input.update(app, |state, cx| state.set_value("", window, cx));
    }
    window.open_dialog(app, move |dialog, _, _| {
        let content_name = name.clone();
        let content_message = message.clone();
        dialog
            .title(ramag_ui::closable_dialog_title(
                "vcs-create-tag-close",
                "新建 Tag",
                |_, _| {},
            ))
            .close_button(false)
            .width(px(520.0))
            .margin_top(px(160.0))
            .content(move |content, _, _| {
                content.child(
                    v_flex()
                        .w_full()
                        .gap(px(8.0))
                        .child(Input::new(&content_name).small())
                        .child(Input::new(&content_message).small()),
                )
            })
            .footer(create_dialog_footer(
                "vcs-create-tag",
                "创建",
                view.clone(),
                |this, cx| this.handle_create_tag(cx),
            ))
    });
}

fn open_create_remote_dialog(view: Entity<VcsView>, window: &mut Window, app: &mut App) {
    let (name, url) = {
        let this = view.read(app);
        (
            this.create_remote_name_input.clone(),
            this.create_remote_url_input.clone(),
        )
    };
    for input in [&name, &url] {
        input.update(app, |state, cx| state.set_value("", window, cx));
    }
    window.open_dialog(app, move |dialog, _, _| {
        let content_name = name.clone();
        let content_url = url.clone();
        dialog
            .title(ramag_ui::closable_dialog_title(
                "vcs-create-remote-close",
                "添加远程仓库",
                |_, _| {},
            ))
            .close_button(false)
            .width(px(560.0))
            .margin_top(px(160.0))
            .content(move |content, _, _| {
                content.child(
                    v_flex()
                        .w_full()
                        .gap(px(8.0))
                        .child(Input::new(&content_name).small())
                        .child(Input::new(&content_url).small()),
                )
            })
            .footer(create_dialog_footer(
                "vcs-create-remote",
                "添加",
                view.clone(),
                |this, cx| this.handle_create_remote(cx),
            ))
    });
}

fn create_dialog_footer(
    id: &'static str,
    label: &'static str,
    view: gpui::Entity<VcsView>,
    submit: impl Fn(&mut VcsView, &mut Context<VcsView>) + 'static,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .justify_end()
        .gap(px(8.0))
        .child(
            ramag_ui::clickable_button(format!("{id}-cancel"))
                .ghost()
                .small()
                .label("取消")
                .on_click(|_: &ClickEvent, window, app| window.close_dialog(app)),
        )
        .child(
            ramag_ui::clickable_button(id)
                .primary()
                .small()
                .label(label)
                .on_click(move |_: &ClickEvent, window, app| {
                    view.update(app, |this, cx| submit(this, cx));
                    window.close_dialog(app);
                }),
        )
}
