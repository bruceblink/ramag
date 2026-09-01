use super::*;
use ramag_ui::PointerDropdownMenu as _;

impl KafkaView {
    pub(crate) fn render_acl_filter_resource_types(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let current = self.acl_filter_resource_type;
        let entity = cx.entity();
        let label = current.map(KafkaAclResourceType::label).unwrap_or("全部");
        ramag_ui::clickable_button("kafka-acl-filter-resource-type")
            .small()
            .w_full()
            .label(format!("{label} ▾"))
            .pointer_dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |mut menu, _, _| {
                for (option, option_label) in [
                    (None, "全部"),
                    (Some(KafkaAclResourceType::Topic), "Topic"),
                    (Some(KafkaAclResourceType::Group), "Group"),
                    (Some(KafkaAclResourceType::Cluster), "Cluster"),
                    (
                        Some(KafkaAclResourceType::TransactionalId),
                        "Transactional ID",
                    ),
                ] {
                    let entity = entity.clone();
                    menu = menu.item(
                        ramag_ui::menu_item(if option == current {
                            format!("✓ {option_label}")
                        } else {
                            option_label.to_string()
                        })
                        .on_click(move |_, _, app| {
                            entity.update(app, |this, cx| {
                                this.acl_filter_resource_type = option;
                                cx.notify();
                            });
                        }),
                    );
                }
                menu
            })
    }

    pub(crate) fn render_acl_filter_pattern_types(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let current = self.acl_filter_pattern_type;
        let entity = cx.entity();
        let label = current.map(KafkaAclPatternType::label).unwrap_or("全部");
        ramag_ui::clickable_button("kafka-acl-filter-pattern-type")
            .small()
            .w_full()
            .label(format!("{label} ▾"))
            .pointer_dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |mut menu, _, _| {
                for (option, option_label) in [
                    (None, "全部"),
                    (Some(KafkaAclPatternType::Literal), "Literal"),
                    (Some(KafkaAclPatternType::Prefixed), "Prefixed"),
                    (Some(KafkaAclPatternType::Match), "Match"),
                ] {
                    let entity = entity.clone();
                    menu = menu.item(
                        ramag_ui::menu_item(if option == current {
                            format!("✓ {option_label}")
                        } else {
                            option_label.to_string()
                        })
                        .on_click(move |_, _, app| {
                            entity.update(app, |this, cx| {
                                this.acl_filter_pattern_type = option;
                                cx.notify();
                            });
                        }),
                    );
                }
                menu
            })
    }

    pub(crate) fn render_acl_filter_operations(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.acl_filter_operation;
        let entity = cx.entity();
        let label = current.map(KafkaAclOperation::label).unwrap_or("全部");
        ramag_ui::clickable_button("kafka-acl-filter-operation")
            .small()
            .w_full()
            .label(format!("{label} ▾"))
            .pointer_dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |mut menu, _, _| {
                for (option, option_label) in [
                    (None, "全部"),
                    (Some(KafkaAclOperation::All), "ALL"),
                    (Some(KafkaAclOperation::Read), "READ"),
                    (Some(KafkaAclOperation::Write), "WRITE"),
                    (Some(KafkaAclOperation::Create), "CREATE"),
                    (Some(KafkaAclOperation::Delete), "DELETE"),
                    (Some(KafkaAclOperation::Alter), "ALTER"),
                    (Some(KafkaAclOperation::Describe), "DESCRIBE"),
                    (Some(KafkaAclOperation::ClusterAction), "CLUSTER_ACTION"),
                    (Some(KafkaAclOperation::DescribeConfigs), "DESCRIBE_CONFIGS"),
                    (Some(KafkaAclOperation::AlterConfigs), "ALTER_CONFIGS"),
                    (Some(KafkaAclOperation::IdempotentWrite), "IDEMPOTENT_WRITE"),
                ] {
                    let entity = entity.clone();
                    menu = menu.item(
                        ramag_ui::menu_item(if option == current {
                            format!("✓ {option_label}")
                        } else {
                            option_label.to_string()
                        })
                        .on_click(move |_, _, app| {
                            entity.update(app, |this, cx| {
                                this.acl_filter_operation = option;
                                cx.notify();
                            });
                        }),
                    );
                }
                menu
            })
    }

    pub(crate) fn render_acl_filter_permissions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.acl_filter_permission;
        let entity = cx.entity();
        let label = current.map(KafkaAclPermission::label).unwrap_or("全部");
        ramag_ui::clickable_button("kafka-acl-filter-permission")
            .small()
            .w_full()
            .label(format!("{label} ▾"))
            .pointer_dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |mut menu, _, _| {
                for (option, option_label) in [
                    (None, "全部"),
                    (Some(KafkaAclPermission::Allow), "ALLOW"),
                    (Some(KafkaAclPermission::Deny), "DENY"),
                ] {
                    let entity = entity.clone();
                    menu = menu.item(
                        ramag_ui::menu_item(if option == current {
                            format!("✓ {option_label}")
                        } else {
                            option_label.to_string()
                        })
                        .on_click(move |_, _, app| {
                            entity.update(app, |this, cx| {
                                this.acl_filter_permission = option;
                                cx.notify();
                            });
                        }),
                    );
                }
                menu
            })
    }

    pub(crate) fn render_acl_resource_types(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.acl_resource_type;
        let entity = cx.entity();
        ramag_ui::clickable_button("kafka-acl-resource-type")
            .small()
            .w_full()
            .label(format!("{} ▾", current.label()))
            .pointer_dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |mut menu, _, _| {
                for option in [
                    KafkaAclResourceType::Topic,
                    KafkaAclResourceType::Group,
                    KafkaAclResourceType::Cluster,
                    KafkaAclResourceType::TransactionalId,
                ] {
                    let entity = entity.clone();
                    menu = menu.item(
                        ramag_ui::menu_item(if option == current {
                            format!("✓ {}", option.label())
                        } else {
                            option.label().to_string()
                        })
                        .on_click(move |_, _, app| {
                            entity.update(app, |this, cx| {
                                this.acl_resource_type = option;
                                cx.notify();
                            });
                        }),
                    );
                }
                menu
            })
    }

    pub(crate) fn render_acl_pattern_types(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.acl_pattern_type;
        let entity = cx.entity();
        ramag_ui::clickable_button("kafka-acl-pattern-type")
            .small()
            .w_full()
            .label(format!("{} ▾", current.label()))
            .pointer_dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |mut menu, _, _| {
                for option in [KafkaAclPatternType::Literal, KafkaAclPatternType::Prefixed] {
                    let entity = entity.clone();
                    menu = menu.item(
                        ramag_ui::menu_item(if option == current {
                            format!("✓ {}", option.label())
                        } else {
                            option.label().to_string()
                        })
                        .on_click(move |_, _, app| {
                            entity.update(app, |this, cx| {
                                this.acl_pattern_type = option;
                                cx.notify();
                            });
                        }),
                    );
                }
                menu
            })
    }

    pub(crate) fn render_acl_operations(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.acl_operation_kind;
        let entity = cx.entity();
        ramag_ui::clickable_button("kafka-acl-operation")
            .small()
            .w_full()
            .label(format!("{} ▾", current.label()))
            .pointer_dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |mut menu, _, _| {
                for option in [
                    KafkaAclOperation::All,
                    KafkaAclOperation::Read,
                    KafkaAclOperation::Write,
                    KafkaAclOperation::Create,
                    KafkaAclOperation::Delete,
                    KafkaAclOperation::Alter,
                    KafkaAclOperation::Describe,
                    KafkaAclOperation::ClusterAction,
                    KafkaAclOperation::DescribeConfigs,
                    KafkaAclOperation::AlterConfigs,
                    KafkaAclOperation::IdempotentWrite,
                ] {
                    let entity = entity.clone();
                    menu = menu.item(
                        ramag_ui::menu_item(if option == current {
                            format!("✓ {}", option.label())
                        } else {
                            option.label().to_string()
                        })
                        .on_click(move |_, _, app| {
                            entity.update(app, |this, cx| {
                                this.acl_operation_kind = option;
                                cx.notify();
                            });
                        }),
                    );
                }
                menu
            })
    }

    pub(crate) fn render_acl_permissions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.acl_permission;
        let entity = cx.entity();
        ramag_ui::clickable_button("kafka-acl-permission")
            .small()
            .w_full()
            .label(format!("{} ▾", current.label()))
            .pointer_dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |mut menu, _, _| {
                for option in [KafkaAclPermission::Allow, KafkaAclPermission::Deny] {
                    let entity = entity.clone();
                    menu = menu.item(
                        ramag_ui::menu_item(if option == current {
                            format!("✓ {}", option.label())
                        } else {
                            option.label().to_string()
                        })
                        .on_click(move |_, _, app| {
                            entity.update(app, |this, cx| {
                                this.acl_permission = option;
                                cx.notify();
                            });
                        }),
                    );
                }
                menu
            })
    }
}
