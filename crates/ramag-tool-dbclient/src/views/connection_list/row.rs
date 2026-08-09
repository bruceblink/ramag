//! 单行连接（整行点击 = 打开；行内编辑/删除独立 emit）
//!
//! driver badge + 名称 + 只读标记 + 版本 / 地址 / 账号固定列对齐 + 编辑/删除按钮。

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div, img, prelude::*, px,
};
use gpui_component::{Sizable as _, button::ButtonVariants as _, h_flex};
use ramag_domain::entities::{ConnectionConfig, DriverKind};

use super::{ConnectionListPanel, ListEvent};

/// 行密度：按面板宽度决定隐藏哪些次要列（800px 窗口固定列已挤占近满）
#[derive(Clone, Copy, PartialEq)]
pub(super) enum RowDensity {
    /// 宽：版本 / 地址 / 账号全显示
    Full,
    /// 中：隐藏版本、账号，保留地址
    Medium,
    /// 窄：仅名称、类型、只读、操作
    Narrow,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn connection_row(
    idx: usize,
    conn: ConnectionConfig,
    is_selected: bool,
    show_sync: bool,
    // 服务端版本（None = 还没拉到 / 拉失败）
    version: Option<String>,
    density: RowDensity,
    border: gpui::Hsla,
    hover_bg: gpui::Hsla,
    accent: gpui::Hsla,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    cx: &mut Context<ConnectionListPanel>,
) -> impl IntoElement {
    let show_version = density == RowDensity::Full;
    let show_address = density != RowDensity::Narrow;
    let show_account = density == RowDensity::Full;
    let kind_label = match conn.driver {
        DriverKind::Mysql => "MySQL",
        DriverKind::Postgres => "PostgreSQL",
        DriverKind::Redis => "Redis",
        DriverKind::Mongodb => "MongoDB",
    };

    // 类型 badge 前的官方品牌彩色 logo（img 渲染，非单色 Icon）
    let brand_icon: Option<&'static str> = ramag_ui::icons::db_brand_icon(match conn.driver {
        DriverKind::Mysql => "mysql",
        DriverKind::Postgres => "postgres",
        DriverKind::Redis => "redis",
        DriverKind::Mongodb => "mongodb",
    });

    // driver 配色（一类一色，便于扫一眼连接列表区分）：
    // MySQL 蓝（主题 accent）/ PostgreSQL 紫 / Redis 红 / MongoDB 绿
    let badge_fg: gpui::Hsla = match conn.driver {
        DriverKind::Mysql => accent,
        DriverKind::Postgres => gpui::hsla(265.0 / 360.0, 0.55, 0.55, 1.0),
        DriverKind::Redis => gpui::hsla(0.0, 0.65, 0.55, 1.0),
        DriverKind::Mongodb => gpui::hsla(140.0 / 360.0, 0.55, 0.45, 1.0),
    };
    let mut badge_bg = badge_fg;
    badge_bg.a = 0.12;

    let row_id = SharedString::from(format!("conn-row-{}-{}", idx, conn.id));
    let edit_id = SharedString::from(format!("conn-edit-{}-{}", idx, conn.id));
    let sync_id = SharedString::from(format!("conn-sync-{}-{}", idx, conn.id));
    let del_id = SharedString::from(format!("conn-del-{}-{}", idx, conn.id));

    let conn_for_open = conn.clone();
    let conn_for_edit = conn.clone();
    let conn_for_sync = conn.clone();
    let conn_id_for_del = conn.id.clone();
    let is_production = conn.production;
    let environment = conn.environment.clone().unwrap_or_default();

    let host_port = format!("{}:{}", conn.host, conn.port);

    // 名字 = host 时（用户没改默认同步），名字列已显示 host:port，地址列留空避免重复
    let name_collapsed_with_host = conn.name == conn.host;
    let primary_label = if name_collapsed_with_host {
        host_port.clone()
    } else {
        conn.name.clone()
    };
    let address_text = if name_collapsed_with_host {
        String::new()
    } else {
        host_port
    };

    // 账号：空段省略，避免 Redis 这类无 user / db 的连接显示无意义的「— @ —」
    let account_text = {
        let user = conn.username.trim();
        let db = conn.database.as_deref().map(str::trim).unwrap_or("");
        match (user.is_empty(), db.is_empty()) {
            (false, false) => format!("{user} @ {db}"),
            (false, true) => user.to_string(),
            (true, false) => db.to_string(),
            (true, true) => String::new(),
        }
    };

    // 类型名已由独立类型列展示，版本列只放纯版本号，避免重复
    let version_text = version.unwrap_or_default();

    // 固定宽度的次要信息列：内容为空也占位，保证各行整列对齐
    let secondary_col = move |w: f32, text: String| {
        div()
            .flex_none()
            .w(px(w))
            .text_xs()
            .text_color(muted_fg)
            .overflow_hidden()
            .text_ellipsis()
            .child(text)
    };

    let danger = gpui::hsla(0.0, 0.7, 0.55, 1.0);
    let mut prod_bg = danger;
    prod_bg.a = 0.15;

    let mut row = h_flex()
        .id(row_id)
        .w_full()
        .items_center()
        .gap(px(12.0))
        .px(px(14.0))
        .py(px(8.0))
        .border_b_1()
        .border_color(border)
        .cursor_pointer()
        .hover(move |this| this.bg(hover_bg))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.handle_click(conn_for_open.clone(), cx);
        }))
        .child(
            div()
                .flex_none()
                .w(px(24.0))
                .flex()
                .justify_center()
                .when_some(brand_icon, |slot, icon| {
                    slot.child(img(icon).size(px(18.0)).flex_none())
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(fg)
                .overflow_hidden()
                .text_ellipsis()
                .child(primary_label),
        )
        .child({
            let slot = div().flex_none().w(px(64.0)).flex().justify_center();
            if environment.trim().is_empty() {
                slot
            } else {
                let (env_fg, env_bg) = environment_badge_colors(&environment, muted_fg);
                slot.child(
                    div()
                        .px(px(6.0))
                        .py(px(1.0))
                        .rounded(px(4.0))
                        .text_xs()
                        .text_color(env_fg)
                        .bg(env_bg)
                        .max_w_full()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(environment),
                )
            }
        })
        // 类型胶囊移到名称之后的固定列（一类一色，保留扫读区分度）
        .child(
            div().flex_none().w(px(84.0)).flex().justify_center().child(
                div()
                    .px(px(8.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .text_xs()
                    .text_color(badge_fg)
                    .bg(badge_bg)
                    .child(kind_label),
            ),
        )
        // 只读徽章槽（固定宽，生产连接显示红色「只读」，否则空白占位 → 整列对齐）
        .child(div().flex_none().w(px(44.0)).flex().justify_center().when(
            is_production,
            move |slot| {
                slot.child(
                    div()
                        .px(px(6.0))
                        .py(px(1.0))
                        .rounded(px(4.0))
                        .text_xs()
                        .text_color(danger)
                        .bg(prod_bg)
                        .child("只读"),
                )
            },
        ))
        // 版本 / 地址 / 账号：按密度隐藏，窄窗口下让位给名称与操作
        .when(show_version, |row| {
            row.child(secondary_col(120.0, version_text))
        })
        .when(show_address, |row| {
            row.child(secondary_col(150.0, address_text))
        })
        .when(show_account, |row| {
            row.child(secondary_col(150.0, account_text))
        })
        // 拦截按钮事件，避免触发父行的打开操作。
        .child(
            h_flex()
                .flex_none()
                .gap(px(4.0))
                .w(px(108.0))
                .justify_end()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when(show_sync, |actions| {
                    actions.child(
                        ramag_ui::clickable_button(sync_id)
                            .ghost()
                            .small()
                            .icon(ramag_ui::icons::database_sync())
                            .tooltip("数据同步")
                            .on_click(cx.listener(move |_this, _: &ClickEvent, _, cx| {
                                cx.emit(ListEvent::RequestSync(conn_for_sync.clone()));
                            })),
                    )
                })
                .child(
                    ramag_ui::clickable_button(edit_id)
                        .ghost()
                        .small()
                        .icon(ramag_ui::icons::pencil())
                        .on_click(cx.listener(move |_this, _: &ClickEvent, _, cx| {
                            cx.emit(ListEvent::RequestEdit(conn_for_edit.clone()));
                        })),
                )
                .child(
                    ramag_ui::clickable_button(del_id)
                        .ghost()
                        .small()
                        .icon(ramag_ui::icons::trash())
                        .on_click(cx.listener(move |_this, _: &ClickEvent, _, cx| {
                            cx.emit(ListEvent::RequestDelete(conn_id_for_del.clone()));
                        })),
                ),
        );

    if is_selected {
        let mut sel_bg = accent;
        sel_bg.a = 0.06;
        row = row.bg(sel_bg);
    }

    row
}

/// 环境徽章配色：dev 绿 / test 琥珀 / prod 红（与只读同域警示色），自定义值用中性灰
fn environment_badge_colors(
    environment: &str,
    fallback_fg: gpui::Hsla,
) -> (gpui::Hsla, gpui::Hsla) {
    let fg = match environment.trim().to_ascii_lowercase().as_str() {
        "dev" => gpui::hsla(140.0 / 360.0, 0.55, 0.42, 1.0),
        "test" => gpui::hsla(35.0 / 360.0, 0.80, 0.45, 1.0),
        "prod" => gpui::hsla(0.0, 0.70, 0.55, 1.0),
        _ => fallback_fg,
    };
    let mut bg = fg;
    bg.a = 0.12;
    (fg, bg)
}
