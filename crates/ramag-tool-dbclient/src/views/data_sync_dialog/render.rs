use gpui::{
    Anchor, ClickEvent, Context, IntoElement, ParentElement, Render, Styled, Window, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, WindowExt as _, button::ButtonVariants as _,
    h_flex, input::Input, scroll::ScrollableElement as _, spinner::Spinner, v_flex,
};
use ramag_domain::entities::{DriverKind, SyncObjectState};
use ramag_ui::PointerDropdownMenu as _;

use super::catalog::connection_label;
use super::{CatalogState, DataSyncDialog, PanelState, RedisMode, value};

impl DataSyncDialog {
    fn render_source_selector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let sources = self.sources.clone();
        let current = self
            .source()
            .map(connection_label)
            .unwrap_or_else(|_| "请选择源连接".into());
        let current_index = self.source_index;
        ramag_ui::clickable_button("sync-source-selector")
            .outline()
            .small()
            .w_full()
            .label(current)
            .dropdown_caret(true)
            .disabled(self.sources.is_empty() || self.state == PanelState::Preflighting)
            .pointer_dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
                for (index, source) in sources.iter().enumerate() {
                    let entity = entity.clone();
                    menu = menu.item(
                        ramag_ui::menu_item(connection_label(source))
                            .checked(Some(index) == current_index)
                            .on_click(move |_: &ClickEvent, _, app| {
                                entity.update(app, |this, cx| this.select_source(index, cx));
                            }),
                    );
                }
                menu
            })
    }

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
        let source_button = ramag_ui::clickable_button("sync-source-scope-selector")
            .outline()
            .small()
            .w_full()
            .label(source_label)
            .dropdown_caret(true)
            .disabled(busy || source_scopes.is_empty())
            .pointer_dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
                for scope in &source_scopes {
                    let scope_for_action = scope.clone();
                    let entity = entity.clone();
                    menu = menu.item(
                        ramag_ui::menu_item(scope.clone())
                            .checked(source_selected.as_deref() == Some(scope.as_str()))
                            .on_click(move |_: &ClickEvent, _, app| {
                                entity.update(app, |this, cx| {
                                    this.select_source_scope(scope_for_action.clone(), cx);
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
                "暂无已有范围"
            } else {
                "选择已有"
            })
            .dropdown_caret(true)
            .disabled(busy || target_scopes.is_empty())
            .pointer_dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
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
        h_flex()
            .w_full()
            .gap(px(10.0))
            .child(field_label(&format!("源 {scope_label}"), source_button).flex_1())
            .child(
                field_label(
                    &format!("目标 {scope_label}（可选择已有或输入新名称）"),
                    h_flex()
                        .w_full()
                        .gap(px(6.0))
                        .child(Input::new(&self.target_scope).disabled(busy).flex_1())
                        .child(target_picker),
                )
                .flex_1(),
            )
    }

    fn render_prefix_field(
        &self,
        label: &str,
        id: &'static str,
        input: &gpui::Entity<gpui_component::input::InputState>,
        suggestions: Vec<String>,
        busy: bool,
        _cx: &mut Context<Self>,
    ) -> gpui::Div {
        let input_for_menu = input.clone();
        let picker = ramag_ui::clickable_button(id)
            .outline()
            .small()
            .label(if suggestions.is_empty() {
                "无可发现前缀"
            } else {
                "选择已发现"
            })
            .dropdown_caret(true)
            .disabled(busy || suggestions.is_empty())
            .pointer_dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
                for suggestion in &suggestions {
                    let suggestion_for_action = suggestion.clone();
                    let input = input_for_menu.clone();
                    menu = menu.item(ramag_ui::menu_item(suggestion.clone()).on_click(
                        move |_: &ClickEvent, window, app| {
                            input.update(app, |input, cx| {
                                input.set_value(&suggestion_for_action, window, cx);
                            });
                        },
                    ));
                }
                menu
            });
        field_label(
            label,
            h_flex()
                .w_full()
                .gap(px(6.0))
                .child(Input::new(input).disabled(busy).flex_1())
                .child(picker),
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
        let mappings = report
            .objects
            .iter()
            .take(12)
            .map(|object| format!("{} → {}", object.mapping.source, object.mapping.target))
            .collect::<Vec<_>>()
            .join("，");
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
                        .child(if report.requires_second_confirmation {
                            "目标已存在：需要二次确认"
                        } else {
                            "目标不存在：将创建后同步"
                        }),
                )
                .child(div().text_xs().child(format!(
                    "源版本 {}；目标版本 {}；新建 {missing}，兼容既有 {existing}",
                    report.source_version, report.target_version
                )))
                .when(!mappings.is_empty(), |panel| {
                    panel.child(div().text_xs().text_color(muted_foreground).child(mappings))
                })
                .children(report.warnings.iter().take(8).map(|warning| {
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
        let muted_foreground = cx.theme().muted_foreground;
        let preflighting = self.state == PanelState::Preflighting;
        let catalog_busy = matches!(
            self.catalog_state,
            CatalogState::LoadingScopes | CatalogState::LoadingObjects
        );
        let busy = preflighting || catalog_busy;
        let controls_disabled = busy || self.source_index.is_none();
        let ready = self.state == PanelState::Ready;
        let is_redis = self.target.driver == DriverKind::Redis;
        let target_text = format!(
            "{}（{}）",
            connection_label(&self.target),
            driver_label(self.target.driver)
        );
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
        let selection_required = (!is_redis && self.selected_objects)
            || (is_redis && self.redis_mode == RedisMode::Keys);
        let can_preflight = self.catalog_state == CatalogState::Ready
            && !self.sources.is_empty()
            && !target_scope_empty
            && (!selection_required || !self.mapping_editors.is_empty());

        v_flex()
            .id("data-sync-dialog-body")
            .w_full()
            .max_h(px(690.0))
            .overflow_y_scrollbar()
            .gap(px(12.0))
            .child(field_label(
                "目标连接（固定）",
                div().text_sm().child(target_text),
            ))
            .child(field_label(
                "源连接（仅同引擎，按地址和库区分）",
                if self.sources.is_empty() {
                    div()
                        .text_sm()
                        .text_color(danger)
                        .child("没有可选源连接")
                        .into_any_element()
                } else {
                    self.render_source_selector(cx).into_any_element()
                },
            ))
            .child(self.render_scope_selectors(controls_disabled, cx))
            .when(!is_redis, |panel| {
                panel.child(field_label(
                    "同步范围",
                    h_flex()
                        .gap(px(6.0))
                        .child(mode_button(
                            "sync-all-objects",
                            &format!("全部（已发现 {} 个）", self.source_objects.len()),
                            !self.selected_objects,
                            controls_disabled,
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.selected_objects = false;
                                this.invalidate(cx);
                            }),
                        ))
                        .child(mode_button(
                            "sync-selected-objects",
                            "选择表 / Collection",
                            self.selected_objects,
                            controls_disabled,
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.selected_objects = true;
                                this.invalidate(cx);
                            }),
                        )),
                ))
            })
            .when(is_redis, |panel| {
                panel.child(field_label(
                    "Redis 范围",
                    h_flex()
                        .gap(px(6.0))
                        .child(redis_mode_button(
                            "sync-redis-db",
                            "整个 DB",
                            RedisMode::Database,
                            self.redis_mode,
                            controls_disabled,
                            cx,
                        ))
                        .child(redis_mode_button(
                            "sync-redis-prefix",
                            "选择 Key 前缀",
                            RedisMode::Prefix,
                            self.redis_mode,
                            controls_disabled,
                            cx,
                        ))
                        .child(redis_mode_button(
                            "sync-redis-keys",
                            "选择 Key",
                            RedisMode::Keys,
                            self.redis_mode,
                            controls_disabled,
                            cx,
                        )),
                ))
            })
            .when(
                (!is_redis && self.selected_objects)
                    || (is_redis && self.redis_mode == RedisMode::Keys),
                |panel| {
                    panel
                        .child(self.render_object_selector(controls_disabled, cx))
                        .child(self.render_mapping_editors(controls_disabled, cx))
                },
            )
            .when(is_redis && self.redis_mode == RedisMode::Prefix, |panel| {
                panel.child(
                    h_flex()
                        .w_full()
                        .gap(px(10.0))
                        .child(
                            self.render_prefix_field(
                                "源前缀（从真实 Key 发现或自定义）",
                                "sync-source-prefix-picker",
                                &self.source_prefix,
                                self.source_prefix_suggestions(),
                                controls_disabled,
                                cx,
                            )
                            .flex_1(),
                        )
                        .child(
                            self.render_prefix_field(
                                "目标前缀（选择已有或输入新前缀）",
                                "sync-target-prefix-picker",
                                &self.target_prefix,
                                self.target_prefix_suggestions(),
                                controls_disabled,
                                cx,
                            )
                            .flex_1(),
                        ),
                )
            })
            .when(
                is_redis && self.redis_mode == RedisMode::Database,
                |panel| {
                    panel.child(field_label(
                        "目标 Key 统一前缀（新前缀，可空）",
                        Input::new(&self.target_prefix).disabled(controls_disabled),
                    ))
                },
            )
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
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.reload_catalog_scopes(cx);
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
                            CatalogState::LoadingScopes => "正在读取两端可选库 / Schema / DB…",
                            _ => "正在读取源对象和目标候选…",
                        }),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        ramag_ui::clickable_button("sync-dialog-cancel")
                            .ghost()
                            .small()
                            .label("关闭")
                            .disabled(preflighting)
                            .on_click(|_: &ClickEvent, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        ramag_ui::clickable_button("sync-dialog-preflight")
                            .outline()
                            .small()
                            .label(if preflighting {
                                "预检中…"
                            } else {
                                "预检"
                            })
                            .disabled(busy || !can_preflight)
                            .when(preflighting, |button| button.icon(Spinner::new().xsmall()))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.preflight(cx);
                            })),
                    )
                    .child(
                        ramag_ui::clickable_button("sync-dialog-start")
                            .primary()
                            .small()
                            .label(
                                if self.prepared.as_ref().is_some_and(|prepared| {
                                    prepared.report().requires_second_confirmation
                                }) {
                                    "二次确认并开始"
                                } else {
                                    "确认并开始"
                                },
                            )
                            .disabled(!ready)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.start(window, cx);
                            })),
                    ),
            )
            .when(!busy && self.state == PanelState::Editing, |panel| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(muted_foreground)
                        .child("先从真实元数据选择源范围，再预检。同步不更新、不覆盖、不删除。"),
                )
            })
            .on_key_down(cx.listener(|_this, _, _window, _cx| {}))
            .when(window.viewport_size().height < px(760.0), |panel| {
                panel.max_h(px(560.0))
            })
    }
}

fn redis_mode_button(
    id: &'static str,
    label: &'static str,
    mode: RedisMode,
    current: RedisMode,
    busy: bool,
    cx: &mut Context<DataSyncDialog>,
) -> impl IntoElement {
    mode_button(
        id,
        label,
        current == mode,
        busy,
        cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.set_redis_mode(mode, cx);
        }),
    )
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
        DriverKind::Redis => "DB 编号",
    }
}

fn driver_label(driver: DriverKind) -> &'static str {
    match driver {
        DriverKind::Mysql => "MySQL",
        DriverKind::Postgres => "PostgreSQL",
        DriverKind::Redis => "Redis",
        DriverKind::Mongodb => "MongoDB",
    }
}

fn field_label(label: &str, child: impl IntoElement) -> gpui::Div {
    v_flex()
        .w_full()
        .gap(px(5.0))
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(label.to_string()),
        )
        .child(child)
}
