//! 单行数据库连接。

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div, img, prelude::*, px,
};
use gpui_component::{
    Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    menu::{ContextMenuExt as _, PopupMenu},
};
use ramag_domain::entities::{ConnectionConfig, DriverKind};

use super::{ConnectionListPanel, ListEvent};

#[derive(Clone, Copy, PartialEq)]
pub(super) enum RowDensity {
    Full,
    Medium,
    Narrow,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn connection_row(
    idx: usize,
    conn: ConnectionConfig,
    is_selected: bool,
    show_sync: bool,
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
        DriverKind::Sqlite => "SQLite",
        DriverKind::Redis => "Redis",
        DriverKind::Mongodb => "MongoDB",
    };

    let brand_icon: Option<&'static str> = ramag_ui::icons::db_brand_icon(match conn.driver {
        DriverKind::Mysql => "mysql",
        DriverKind::Postgres => "postgres",
        DriverKind::Sqlite => "sqlite",
        DriverKind::Redis => "redis",
        DriverKind::Mongodb => "mongodb",
    });

    let badge_fg: gpui::Hsla = match conn.driver {
        DriverKind::Mysql => accent,
        DriverKind::Postgres => gpui::hsla(265.0 / 360.0, 0.55, 0.55, 1.0),
        DriverKind::Sqlite => gpui::hsla(200.0 / 360.0, 0.60, 0.50, 1.0),
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
    let conn_for_duplicate = conn.clone();
    let conn_id_for_del = conn.id.clone();
    let entity_for_menu = cx.entity().clone();
    let is_production = conn.production;
    let environment = conn.environment.clone().unwrap_or_default();

    let host_port = format!("{}:{}", conn.host, conn.port);

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

    let version_text = version.unwrap_or_default();

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
                        .child(ramag_ui::PRODUCTION_BADGE_LABEL),
                )
            },
        ))
        .when(show_version, |row| {
            row.child(secondary_col(120.0, version_text))
        })
        .when(show_address, |row| {
            row.child(secondary_col(150.0, address_text))
        })
        .when(show_account, |row| {
            row.child(secondary_col(150.0, account_text))
        })
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
                        .tooltip("编辑")
                        .on_click(cx.listener(move |_this, _: &ClickEvent, _, cx| {
                            cx.emit(ListEvent::RequestEdit(conn_for_edit.clone()));
                        })),
                )
                .child(
                    ramag_ui::clickable_button(del_id)
                        .ghost()
                        .small()
                        .icon(ramag_ui::icons::trash())
                        .tooltip("删除")
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

    row.context_menu(move |menu: PopupMenu, _, _| {
        let entity = entity_for_menu.clone();
        let connection = conn_for_duplicate.clone();
        menu.item(ramag_ui::menu_item("Duplicate").on_click(move |_, _, app| {
            entity.update(app, |_this, cx| {
                cx.emit(ListEvent::RequestDuplicate(connection.clone()));
            });
        }))
    })
}

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
