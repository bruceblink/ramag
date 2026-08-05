use gpui::{
    Anchor, ClickEvent, Context, IntoElement, ParentElement, Render, Styled, Window, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, button::ButtonVariants as _, h_flex, input::Input,
    scroll::ScrollableElement as _, spinner::Spinner, v_flex,
};
use ramag_domain::entities::{DriverKind, SyncObjectState};
use ramag_ui::PointerDropdownMenu as _;

use super::layout_sections::clipped_dropdown_button;
use super::{CatalogState, DROPDOWN_MENU_MAX_HEIGHT, DataSyncDialog, PanelState, value};

impl DataSyncDialog {
    fn render_scope_selectors(&self, busy: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let source_scopes = self.source_scopes.clone();
        let source_selected = self.source_scope.clone();
        let source_label = source_selected.clone().unwrap_or_else(|| {
            if self.catalog_state == CatalogState::AwaitingSource {
                "请先选择源连接".into()
            } else {
                "加载可选范围…".into()
            }
        });
        let source_button = clipped_dropdown_button(
            "sync-source-scope-selector",
            "sync-source-scope-text",
            source_label,
        )
        .disabled(busy || source_scopes.is_empty())
        .pointer_dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
            menu = menu.scrollable(true).max_h(px(DROPDOWN_MENU_MAX_HEIGHT));
            for scope in &source_scopes {
                let scope_for_action = scope.clone();
                let entity = entity.clone();
                menu = menu.item(
                    ramag_ui::menu_item(scope.clone())
                        .checked(source_selected.as_deref() == Some(scope.as_str()))
                        .on_click(move |_: &ClickEvent, window, app| {
                            entity.update(app, |this, cx| {
                                this.select_source_scope(scope_for_action.clone(), window, cx);
                            });
                        }),
                );
            }
            menu
        });

        let entity = cx.entity();
        let target_scopes = self.target_scopes.clone();
        let target_selected = value(&self.target_scope, cx);
        let target_picker = ramag_ui::clickable_button("sync-target-scope-selector")
            .outline()
            .small()
            .label(if target_scopes.is_empty() {
                "无已有"
            } else {
                "选择已有"
            })
            .dropdown_caret(true)
            .disabled(busy || target_scopes.is_empty())
            .pointer_dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
                menu = menu.scrollable(true).max_h(px(DROPDOWN_MENU_MAX_HEIGHT));
                for scope in &target_scopes {
                    let scope_for_action = scope.clone();
                    let entity = entity.clone();
                    menu = menu.item(
                        ramag_ui::menu_item(scope.clone())
                            .checked(target_selected == *scope)
                            .on_click(move |_: &ClickEvent, window, app| {
                                entity.update(app, |this, cx| {
                                    this.select_target_scope(scope_for_action.clone(), window, cx);
                                });
                            }),
                    );
                }
                menu
            });

        let scope_label = scope_label(self.target.driver);
        let target_label = format!("目标 {scope_label}");
        h_flex()
            .w_full()
            .min_w_0()
            .items_end()
            .gap(px(10.0))
            .child(
                field_label(&format!("来源 {scope_label}"), source_button)
                    .flex_1()
                    .overflow_hidden(),
            )
            .child(
                field_label(
                    &target_label,
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap(px(6.0))
                        .child(
                            Input::new(&self.target_scope)
                                .disabled(busy)
                                .flex_1()
                                .min_w_0(),
                        )
                        .child(div().flex_none().child(target_picker)),
                )
                .flex_1()
                .overflow_hidden(),
            )
    }

    fn render_report(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let prepared = self.prepared.as_ref()?;
        let report = prepared.report();
        let warning_color = cx.theme().warning;
        let border = cx.theme().border;
        let muted_foreground = cx.theme().muted_foreground;
        let existing = report
            .objects
            .iter()
            .filter(|object| matches!(object.state, SyncObjectState::ExistingCompatible))
            .count();
        let missing = report
            .objects
            .iter()
            .filter(|object| matches!(object.state, SyncObjectState::Missing))
            .count();
        let renamed = report
            .objects
            .iter()
            .filter(|object| object.mapping.source != object.mapping.target)
            .take(3)
            .map(|object| format!("{} → {}", object.mapping.source, object.mapping.target))
            .collect::<Vec<_>>()
            .join("、");
        let renamed_count = report
            .objects
            .iter()
            .filter(|object| object.mapping.source != object.mapping.target)
            .count();
        let total = report.objects_total.unwrap_or(report.objects.len() as u64);
        let summary = [
            target_scope_summary(report.engine, report.target_scope_exists),
            object_change_summary(total, missing, existing),
            format!("版本 {} → {}", report.source_version, report.target_version),
        ]
        .join(" · ");
        Some(
            v_flex()
                .w_full()
                .gap(px(6.0))
                .p(px(10.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(if report.requires_second_confirmation {
                    warning_color
                } else {
                    border
                })
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("预检通过"),
                )
                .child(div().text_xs().child(summary))
                .when(!renamed.is_empty(), |panel| {
                    let suffix = if renamed_count > 3 {
                        format!(" 等 {renamed_count} 项")
                    } else {
                        String::new()
                    };
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(muted_foreground)
                            .child(format!("重命名：{renamed}{suffix}")),
                    )
                })
                .children(report.warnings.iter().take(3).map(|warning| {
                    div()
                        .text_xs()
                        .text_color(warning_color)
                        .child(format!("注意：{warning}"))
                }))
                .into_any_element(),
        )
    }
}

impl Render for DataSyncDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let danger = cx.theme().danger;
        let warning = cx.theme().warning;
        let preflighting = self.state == PanelState::Preflighting;
        let catalog_busy = matches!(
            self.catalog_state,
            CatalogState::LoadingScopes | CatalogState::LoadingObjects
        );
        let busy = preflighting || catalog_busy;
        let controls_disabled = busy || self.source_index.is_none();
        let ready = self.state == PanelState::Ready;
        let report = self.render_report(cx);
        let error = match &self.state {
            PanelState::Error(error) => Some(error.clone()),
            _ => None,
        };
        let catalog_error = match &self.catalog_state {
            CatalogState::Error(error) => Some(error.clone()),
            _ => None,
        };
        let target_scope_empty = value(&self.target_scope, cx).is_empty();
        let selection_required = self.selected_objects;
        let can_preflight = self.catalog_state == CatalogState::Ready
            && !self.sources.is_empty()
            && !target_scope_empty
            && (!selection_required || !self.mapping_editors.is_empty());
        let footer = self.render_footer(preflighting, busy, can_preflight, ready, cx);
        let content_max_height = if window.viewport_size().height < px(760.0) {
            px(440.0)
        } else if window.viewport_size().height < px(1000.0) {
            px(620.0)
        } else {
            px(700.0)
        };

        let content = v_flex()
            .id("data-sync-dialog-scroll")
            .w_full()
            .max_h(content_max_height)
            .overflow_y_scrollbar()
            .gap(px(10.0))
            .child(self.render_safety_warning(cx))
            .child(self.render_connection_section(busy, cx))
            .child(self.render_scope_selectors(controls_disabled, cx))
            .child(field_label(
                "数据范围",
                h_flex()
                    .gap(px(6.0))
                    .child(mode_button(
                        "sync-all-objects",
                        &format!("全部 {} 个", self.source_objects.len()),
                        !self.selected_objects,
                        controls_disabled,
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.selected_objects = false;
                            this.invalidate(cx);
                        }),
                    ))
                    .child(mode_button(
                        "sync-selected-objects",
                        "选择部分",
                        self.selected_objects,
                        controls_disabled,
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.selected_objects = true;
                            this.invalidate(cx);
                        }),
                    )),
            ))
            .when(self.selected_objects, |panel| {
                panel.child(self.render_object_selector(controls_disabled, cx))
            })
            .when(self.catalog_truncated, |panel| {
                panel.child(
                    div().text_xs().text_color(warning).child(
                        "对象目录超过 10,000 个，仅选择器被截断；“全部”同步仍处理完整范围。",
                    ),
                )
            })
            .when_some(catalog_error, |panel, error| {
                panel.child(
                    h_flex()
                        .w_full()
                        .gap(px(8.0))
                        .child(div().text_sm().text_color(danger).child(error))
                        .child(
                            ramag_ui::clickable_button("sync-catalog-retry")
                                .outline()
                                .small()
                                .label("重新读取")
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.reload_catalog_scopes(window, cx);
                                })),
                        ),
                )
            })
            .when_some(error, |panel, error| {
                panel.child(div().text_sm().text_color(danger).child(error))
            })
            .when_some(report, |panel, report| panel.child(report))
            .when(catalog_busy, |panel| {
                panel.child(
                    h_flex()
                        .gap(px(6.0))
                        .text_sm()
                        .child(Spinner::new().xsmall())
                        .child(match self.catalog_state {
                            CatalogState::LoadingScopes => "正在读取两端可选 Database / Schema…",
                            _ => "正在读取源对象和目标候选…",
                        }),
                )
            });

        v_flex()
            .id("data-sync-dialog-body")
            .w_full()
            .gap(px(10.0))
            .child(content)
            .child(footer)
    }
}

fn mode_button(
    id: &'static str,
    label: &str,
    selected: bool,
    disabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    ramag_ui::clickable_button(id)
        .small()
        .label(label.to_string())
        .disabled(disabled)
        .when(selected, |button| button.primary())
        .when(!selected, |button| button.outline())
        .on_click(on_click)
}

fn scope_label(driver: DriverKind) -> &'static str {
    match driver {
        DriverKind::Mysql => "Database",
        DriverKind::Postgres => "Schema",
        DriverKind::Mongodb => "Database",
        DriverKind::Redis => "不支持",
    }
}

fn target_scope_summary(driver: DriverKind, exists: bool) -> String {
    format!(
        "目标 {}：{}",
        scope_label(driver),
        if exists { "已存在" } else { "将新建" }
    )
}

fn object_change_summary(total: u64, missing: usize, existing: usize) -> String {
    let mut changes = Vec::with_capacity(2);
    if missing > 0 {
        changes.push(format!("新建 {missing}"));
    }
    if existing > 0 {
        changes.push(format!("复用已有 {existing}"));
    }
    if changes.is_empty() {
        format!("对象 {total} 个")
    } else {
        format!("对象 {total} 个（{}）", changes.join("，"))
    }
}

fn field_label(label: &str, child: impl IntoElement) -> gpui::Div {
    v_flex()
        .w_full()
        .min_w_0()
        .gap(px(5.0))
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(label.to_string()),
        )
        .child(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_summary_distinguishes_scope_and_objects() {
        assert_eq!(
            target_scope_summary(DriverKind::Mysql, true),
            "目标 Database：已存在"
        );
        assert_eq!(object_change_summary(1, 1, 0), "对象 1 个（新建 1）");
        assert_eq!(
            object_change_summary(3, 1, 2),
            "对象 3 个（新建 1，复用已有 2）"
        );
        assert_eq!(
            target_scope_summary(DriverKind::Postgres, false),
            "目标 Schema：将新建"
        );
    }
}
