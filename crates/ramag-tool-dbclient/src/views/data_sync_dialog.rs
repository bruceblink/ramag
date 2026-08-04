//! 连接同步配置与预检确认面板。目标固定为列表中点击的当前连接。

use std::sync::Arc;

use gpui::{
    ClickEvent, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window,
    div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputEvent, InputState},
    spinner::Spinner,
    v_flex,
};
use ramag_app::{DataSyncConfirmation, DataSyncService, PreparedDataSync};
use ramag_domain::entities::{
    ConnectionConfig, DataSyncRequest, DataSyncScope, DataSyncTaskId, DriverKind, MongoSyncScope,
    RedisKeyMapping, RedisSyncScope, SqlSyncScope, SyncObjectMapping, SyncObjectSelection,
    SyncObjectState,
};
use ramag_domain::error::{DomainError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedisMode {
    Database,
    Prefix,
    Keys,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PanelState {
    Editing,
    Preflighting,
    Ready,
    Error(String),
}

pub struct DataSyncDialog {
    service: Arc<DataSyncService>,
    target: ConnectionConfig,
    sources: Vec<ConnectionConfig>,
    source_index: usize,
    selected_objects: bool,
    redis_mode: RedisMode,
    source_scope: Entity<InputState>,
    target_scope: Entity<InputState>,
    mappings: Entity<InputState>,
    source_prefix: Entity<InputState>,
    target_prefix: Entity<InputState>,
    state: PanelState,
    prepared: Option<PreparedDataSync>,
    _subscriptions: Vec<Subscription>,
}

impl DataSyncDialog {
    pub fn new(
        service: Arc<DataSyncService>,
        target: ConnectionConfig,
        all_connections: &[ConnectionConfig],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut sources: Vec<_> = all_connections
            .iter()
            .filter(|connection| connection.id != target.id && connection.driver == target.driver)
            .cloned()
            .collect();
        sources.sort_by(|left, right| left.name.cmp(&right.name));
        let source_default = sources
            .first()
            .and_then(|connection| connection.database.clone())
            .unwrap_or_else(|| default_scope(target.driver).to_string());
        let target_default = target
            .database
            .clone()
            .unwrap_or_else(|| default_scope(target.driver).to_string());
        let source_scope = input(window, cx, &source_default, "源 Database / Schema / DB");
        let target_scope = input(window, cx, &target_default, "目标 Database / Schema / DB");
        let mappings = input(window, cx, "", "例如 users=users_copy, orders=orders");
        let source_prefix = input(window, cx, "", "源 Key 前缀");
        let target_prefix = input(window, cx, "", "目标 Key 前缀（可空）");
        let mut subscriptions = Vec::new();
        for field in [
            &source_scope,
            &target_scope,
            &mappings,
            &source_prefix,
            &target_prefix,
        ] {
            subscriptions.push(cx.subscribe(field, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.invalidate(cx);
                }
            }));
        }
        Self {
            service,
            target,
            sources,
            source_index: 0,
            selected_objects: false,
            redis_mode: RedisMode::Database,
            source_scope,
            target_scope,
            mappings,
            source_prefix,
            target_prefix,
            state: PanelState::Editing,
            prepared: None,
            _subscriptions: subscriptions,
        }
    }

    fn invalidate(&mut self, cx: &mut Context<Self>) {
        if self.state != PanelState::Preflighting {
            self.state = PanelState::Editing;
            self.prepared = None;
            cx.notify();
        }
    }

    fn source(&self) -> Result<&ConnectionConfig> {
        self.sources.get(self.source_index).ok_or_else(|| {
            DomainError::InvalidConfig("没有可用的同引擎源连接，请先新建另一个连接".into())
        })
    }

    fn build_request(&self, cx: &Context<Self>) -> Result<DataSyncRequest> {
        let source = self.source()?;
        let source_scope = value(&self.source_scope, cx);
        let target_scope = value(&self.target_scope, cx);
        let scope = match self.target.driver {
            DriverKind::Mysql | DriverKind::Postgres => DataSyncScope::Sql(SqlSyncScope {
                source_namespace: source_scope,
                target_namespace: target_scope,
                tables: self.object_selection(cx)?,
            }),
            DriverKind::Mongodb => DataSyncScope::Mongo(MongoSyncScope {
                source_database: source_scope,
                target_database: target_scope,
                collections: self.object_selection(cx)?,
            }),
            DriverKind::Redis => {
                let source_db = parse_redis_db(&source_scope, "源 DB")?;
                let target_db = parse_redis_db(&target_scope, "目标 DB")?;
                DataSyncScope::Redis(match self.redis_mode {
                    RedisMode::Database => RedisSyncScope::Database {
                        source_db,
                        target_db,
                        target_prefix: value(&self.target_prefix, cx),
                    },
                    RedisMode::Prefix => RedisSyncScope::Prefix {
                        source_db,
                        target_db,
                        source_prefix: value(&self.source_prefix, cx),
                        target_prefix: value(&self.target_prefix, cx),
                    },
                    RedisMode::Keys => RedisSyncScope::Keys {
                        source_db,
                        target_db,
                        mappings: parse_mappings(&value(&self.mappings, cx))?
                            .into_iter()
                            .map(|mapping| RedisKeyMapping {
                                source: mapping.source,
                                target: mapping.target,
                            })
                            .collect(),
                    },
                })
            }
        };
        Ok(DataSyncRequest {
            task_id: DataSyncTaskId::new(),
            source_connection_id: source.id.clone(),
            target_connection_id: self.target.id.clone(),
            engine: self.target.driver,
            scope,
        })
    }

    fn object_selection(&self, cx: &Context<Self>) -> Result<SyncObjectSelection> {
        if self.selected_objects {
            Ok(SyncObjectSelection::Selected(parse_mappings(&value(
                &self.mappings,
                cx,
            ))?))
        } else {
            Ok(SyncObjectSelection::All)
        }
    }

    fn preflight(&mut self, cx: &mut Context<Self>) {
        if self.state == PanelState::Preflighting {
            return;
        }
        let request = match self.build_request(cx) {
            Ok(request) => request,
            Err(error) => {
                self.state = PanelState::Error(error.message().to_string());
                cx.notify();
                return;
            }
        };
        self.state = PanelState::Preflighting;
        self.prepared = None;
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = service.preflight(request).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(prepared) => {
                        this.prepared = Some(prepared);
                        this.state = PanelState::Ready;
                    }
                    Err(error) => {
                        this.prepared = None;
                        this.state = PanelState::Error(error.message().to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prepared) = self.prepared.take() else {
            self.state = PanelState::Error("预检结果不存在，请重新预检".into());
            cx.notify();
            return;
        };
        let confirmation = if prepared.report().requires_second_confirmation {
            DataSyncConfirmation::ContinueWithExistingTargets
        } else {
            DataSyncConfirmation::CreateMissingTargets
        };
        match self.service.start(prepared, confirmation) {
            Ok(started) => {
                let service = self.service.clone();
                window.close_dialog(cx);
                cx.spawn(async move |_, _| service.execute(started).await)
                    .detach();
            }
            Err(error) => {
                self.state = PanelState::Error(error.message().to_string());
                cx.notify();
            }
        }
    }

    fn render_source_selector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = h_flex().w_full().flex_wrap().gap(px(6.0));
        for (index, source) in self.sources.iter().enumerate() {
            let selected = index == self.source_index;
            row = row.child(
                ramag_ui::clickable_button(gpui::SharedString::from(format!(
                    "sync-source-{}",
                    source.id
                )))
                .small()
                .label(source.name.clone())
                .disabled(self.state == PanelState::Preflighting)
                .when(selected, |button| button.primary())
                .when(!selected, |button| button.outline())
                .on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.source_index = index;
                        let default = this
                            .sources
                            .get(index)
                            .and_then(|source| source.database.as_deref())
                            .unwrap_or_else(|| default_scope(this.target.driver));
                        this.source_scope.update(cx, |input, cx| {
                            input.set_value(default, window, cx);
                        });
                        this.invalidate(cx);
                    },
                )),
            );
        }
        row
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
        let muted_foreground = cx.theme().muted_foreground;
        let busy = self.state == PanelState::Preflighting;
        let ready = self.state == PanelState::Ready;
        let is_redis = self.target.driver == DriverKind::Redis;
        let scope_label = match self.target.driver {
            DriverKind::Mysql => "Database",
            DriverKind::Postgres => "Schema",
            DriverKind::Mongodb => "Database",
            DriverKind::Redis => "DB 编号",
        };
        let target_text = format!(
            "{}（{}）",
            self.target.name,
            driver_label(self.target.driver)
        );
        let report = self.render_report(cx);
        let error = match &self.state {
            PanelState::Error(error) => Some(error.clone()),
            _ => None,
        };

        v_flex()
            .id("data-sync-dialog-body")
            .w_full()
            .gap(px(12.0))
            .child(field_label(
                "目标连接（固定）",
                div().text_sm().child(target_text),
            ))
            .child(field_label(
                "源连接（仅同引擎）",
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
            .child(
                h_flex()
                    .w_full()
                    .gap(px(10.0))
                    .child(
                        field_label(
                            &format!("源 {scope_label}"),
                            Input::new(&self.source_scope).disabled(busy),
                        )
                        .flex_1(),
                    )
                    .child(
                        field_label(
                            &format!("目标 {scope_label}"),
                            Input::new(&self.target_scope).disabled(busy),
                        )
                        .flex_1(),
                    ),
            )
            .when(!is_redis, |panel| {
                panel
                    .child(field_label(
                        "同步范围",
                        h_flex()
                            .gap(px(6.0))
                            .child(mode_button(
                                "sync-all-objects",
                                "全部表 / Collection",
                                !self.selected_objects,
                                busy,
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.selected_objects = false;
                                    this.invalidate(cx);
                                }),
                            ))
                            .child(mode_button(
                                "sync-selected-objects",
                                "指定对象",
                                self.selected_objects,
                                busy,
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.selected_objects = true;
                                    this.invalidate(cx);
                                }),
                            )),
                    ))
                    .when(self.selected_objects, |panel| {
                        panel.child(field_label(
                            "对象映射（逗号分隔，源=目标）",
                            Input::new(&self.mappings).disabled(busy),
                        ))
                    })
            })
            .when(is_redis, |panel| {
                panel
                    .child(field_label(
                        "Redis 范围",
                        h_flex()
                            .gap(px(6.0))
                            .child(self.redis_mode_button(
                                "sync-redis-db",
                                "整个 DB",
                                RedisMode::Database,
                                busy,
                                cx,
                            ))
                            .child(self.redis_mode_button(
                                "sync-redis-prefix",
                                "Key 前缀",
                                RedisMode::Prefix,
                                busy,
                                cx,
                            ))
                            .child(self.redis_mode_button(
                                "sync-redis-keys",
                                "指定 Key",
                                RedisMode::Keys,
                                busy,
                                cx,
                            )),
                    ))
                    .when(self.redis_mode == RedisMode::Prefix, |panel| {
                        panel.child(
                            h_flex()
                                .gap(px(10.0))
                                .child(
                                    field_label(
                                        "源前缀",
                                        Input::new(&self.source_prefix).disabled(busy),
                                    )
                                    .flex_1(),
                                )
                                .child(
                                    field_label(
                                        "目标前缀",
                                        Input::new(&self.target_prefix).disabled(busy),
                                    )
                                    .flex_1(),
                                ),
                        )
                    })
                    .when(self.redis_mode == RedisMode::Database, |panel| {
                        panel.child(field_label(
                            "目标 Key 统一前缀（可空）",
                            Input::new(&self.target_prefix).disabled(busy),
                        ))
                    })
                    .when(self.redis_mode == RedisMode::Keys, |panel| {
                        panel.child(field_label(
                            "Key 映射（逗号分隔，源=目标）",
                            Input::new(&self.mappings).disabled(busy),
                        ))
                    })
            })
            .when_some(error, |panel, error| {
                panel.child(div().text_sm().text_color(danger).child(error))
            })
            .when_some(report, |panel, report| panel.child(report))
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
                            .disabled(busy)
                            .on_click(|_: &ClickEvent, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        ramag_ui::clickable_button("sync-dialog-preflight")
                            .outline()
                            .small()
                            .label(if busy { "预检中…" } else { "重新预检" })
                            .disabled(busy || self.sources.is_empty())
                            .when(busy, |button| button.icon(Spinner::new().xsmall()))
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
                        .child("先预检。同步只新增目标中不存在的记录，不更新、不覆盖、不删除。"),
                )
            })
            .when(busy, |panel| {
                panel.child(
                    h_flex()
                        .gap(px(6.0))
                        .text_sm()
                        .child(Spinner::new().xsmall())
                        .child("正在读取两端结构和目标状态…"),
                )
            })
            .on_key_down(cx.listener(|_this, _, _window, _cx| {
                // 表单输入保持默认键盘行为。
            }))
            .when(window.viewport_size().height < px(600.0), |panel| {
                panel.max_h(px(430.0)).overflow_y_scroll()
            })
    }
}

impl DataSyncDialog {
    fn redis_mode_button(
        &self,
        id: &'static str,
        label: &'static str,
        mode: RedisMode,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        mode_button(
            id,
            label,
            self.redis_mode == mode,
            busy,
            cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.redis_mode = mode;
                this.invalidate(cx);
            }),
        )
    }
}

fn input(
    window: &mut Window,
    cx: &mut Context<DataSyncDialog>,
    default: &str,
    placeholder: &'static str,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .validate(|text, _| text.len() <= 16 * 1024)
            .default_value(default)
            .placeholder(placeholder)
    })
}

fn value(input: &Entity<InputState>, cx: &Context<DataSyncDialog>) -> String {
    input.read(cx).value().trim().to_string()
}

fn parse_redis_db(value: &str, label: &str) -> Result<u8> {
    value
        .parse::<u8>()
        .map_err(|_| DomainError::InvalidConfig(format!("{label} 必须是 0-255 的整数")))
}

fn parse_mappings(value: &str) -> Result<Vec<SyncObjectMapping>> {
    if value.trim().is_empty() {
        return Err(DomainError::InvalidConfig("指定对象映射不能为空".into()));
    }
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| {
            let (source, target) = item.split_once('=').unwrap_or((item, item));
            let source = source.trim();
            let target = target.trim();
            if source.is_empty() || target.is_empty() {
                return Err(DomainError::InvalidConfig(format!(
                    "对象映射格式错误：{item}"
                )));
            }
            Ok(SyncObjectMapping {
                source: source.into(),
                target: target.into(),
            })
        })
        .collect()
}

fn default_scope(driver: DriverKind) -> &'static str {
    match driver {
        DriverKind::Postgres => "public",
        DriverKind::Redis => "0",
        DriverKind::Mysql | DriverKind::Mongodb => "",
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

fn mode_button(
    id: &'static str,
    label: &'static str,
    selected: bool,
    disabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    ramag_ui::clickable_button(id)
        .small()
        .label(label)
        .disabled(disabled)
        .when(selected, |button| button.primary())
        .when(!selected, |button| button.outline())
        .on_click(on_click)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_parser_supports_rename_and_identity_mapping() {
        let mappings = parse_mappings("users=users_copy, orders").expect("映射应合法");
        assert_eq!(mappings[0].source, "users");
        assert_eq!(mappings[0].target, "users_copy");
        assert_eq!(mappings[1].source, "orders");
        assert_eq!(mappings[1].target, "orders");
    }

    #[test]
    fn mapping_parser_rejects_empty_side() {
        assert!(parse_mappings("users=").is_err());
        assert!(parse_mappings("").is_err());
    }
}
