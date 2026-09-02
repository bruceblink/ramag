use super::*;
use ramag_domain::entities::KafkaAclFilter;

impl KafkaView {
    pub(crate) fn invalidate_acl_request(&mut self) {
        self.acl_request_id = self.acl_request_id.wrapping_add(1);
        self.loading_acls = false;
    }

    pub(crate) fn invalidate_acl_operation(&mut self) {
        self.acl_operation_id = self.acl_operation_id.wrapping_add(1);
        self.acl_operation = false;
    }

    pub(crate) fn clear_acl_snapshot(&mut self) {
        self.invalidate_acl_request();
        self.acls.clear();
        self.selected_acl = None;
        self.acls_loaded = false;
        self.acl_error = None;
    }

    /// 清空当前集群的 ACL 快照和表单，避免切换集群时短暂展示旧规则。
    pub(crate) fn reset_acl_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_acl_snapshot();
        self.invalidate_acl_operation();
        self.acl_filter_resource_type = None;
        self.acl_filter_pattern_type = None;
        self.acl_filter_operation = None;
        self.acl_filter_permission = None;
        self.acl_resource_type = KafkaAclResourceType::Topic;
        self.acl_pattern_type = KafkaAclPatternType::Literal;
        self.acl_operation_kind = KafkaAclOperation::Read;
        self.acl_permission = KafkaAclPermission::Allow;
        set_value(&self.acl_principal_filter, "", window, cx);
        set_value(&self.acl_host_filter, "", window, cx);
        set_value(&self.acl_resource_name_filter, "", window, cx);
        set_value(&self.acl_principal, "", window, cx);
        set_value(&self.acl_host, "*", window, cx);
        set_value(&self.acl_resource_name, "", window, cx);
    }

    fn current_acl_filter(&self, cx: &App) -> KafkaAclFilter {
        KafkaAclFilter {
            principal: optional_value(&self.acl_principal_filter, cx),
            host: optional_value(&self.acl_host_filter, cx),
            resource_type: self.acl_filter_resource_type,
            resource_name: optional_value(&self.acl_resource_name_filter, cx),
            pattern_type: self.acl_filter_pattern_type,
            operation: self.acl_filter_operation,
            permission: self.acl_filter_permission,
        }
    }

    fn current_acl(&self, cx: &App) -> Result<KafkaAcl, String> {
        let principal = optional_value(&self.acl_principal, cx)
            .ok_or_else(|| "ACL Principal 不能为空".to_string())?;
        let host = optional_value(&self.acl_host, cx).unwrap_or_else(|| "*".into());
        let resource_name = optional_value(&self.acl_resource_name, cx)
            .ok_or_else(|| "ACL Resource Name 不能为空".to_string())?;
        let mut acl = KafkaAcl::new(
            principal,
            self.acl_resource_type,
            resource_name,
            self.acl_pattern_type,
            self.acl_operation_kind,
            self.acl_permission,
        );
        acl.host = host;
        acl.validate()?;
        Ok(acl)
    }

    pub(crate) fn load_acls(
        &mut self,
        config: KafkaClusterConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading_acls
            || self.acl_operation
            || self.loading_runtime
            || self.saving
            || self.deleting
            || self.selected_cluster_id.as_ref() != Some(&config.id)
        {
            return;
        }
        let filter = self.current_acl_filter(cx);
        if let Err(error) = filter.validate() {
            self.notice = Some((error, true));
            cx.notify();
            return;
        }
        self.acl_request_id = self.acl_request_id.wrapping_add(1);
        let request_id = self.acl_request_id;
        let cluster_id = config.id.clone();
        let service = self.service.clone();
        self.loading_acls = true;
        self.acl_error = None;
        self.notice = Some(("正在读取 Kafka ACL…".into(), false));
        cx.spawn_in(window, async move |this, cx| {
            let result = service.list_acls(&config, &filter).await;
            let _ = this.update_in(cx, |this, _window, cx| {
                if this.acl_request_id != request_id
                    || this.selected_cluster_id.as_ref() != Some(&cluster_id)
                {
                    return;
                }
                this.loading_acls = false;
                match result {
                    Ok(acls) => {
                        let selected = this
                            .selected_acl
                            .clone()
                            .filter(|selected| acls.iter().any(|acl| acl == selected));
                        let count = acls.len();
                        this.acls = acls;
                        this.selected_acl = selected.or_else(|| this.acls.first().cloned());
                        this.acls_loaded = true;
                        this.acl_error = None;
                        this.notice = Some((format!("已读取 {count} 条 Kafka ACL"), false));
                    }
                    Err(error) => {
                        this.acls.clear();
                        this.selected_acl = None;
                        this.acls_loaded = false;
                        this.acl_error = Some(error.user_message());
                        this.mark_runtime_failure("读取 Kafka ACL", &error);
                        this.notice = Some((
                            format!("读取 Kafka ACL 失败：{}", error.user_message()),
                            true,
                        ));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn select_acl(&mut self, acl: KafkaAcl, cx: &mut Context<Self>) {
        if self.acls.iter().any(|candidate| candidate == &acl) {
            self.selected_acl = Some(acl);
            cx.notify();
        }
    }

    pub(crate) fn begin_create_acl(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.acl_operation
            || self.loading_acls
            || self.loading_runtime
            || self.saving
            || self.deleting
            || !self.read_only.allows_admin()
        {
            return;
        }
        let Some(config) = self.selected_config() else {
            return;
        };
        let acl = match self.current_acl(cx) {
            Ok(acl) => acl,
            Err(error) => {
                self.notice = Some((error, true));
                cx.notify();
                return;
            }
        };
        let description = format_acl_confirmation("创建", &acl);
        let view = cx.entity();
        ramag_ui::open_confirm(
            "创建 Kafka ACL？",
            description,
            "创建",
            false,
            move |window, app| {
                view.update(app, |this, cx| {
                    this.execute_acl_operation(config, acl, true, window, cx);
                });
            },
            window,
            cx,
        );
    }

    pub(crate) fn begin_delete_acl(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.acl_operation
            || self.loading_acls
            || self.loading_runtime
            || self.saving
            || self.deleting
            || !self.read_only.allows_admin()
        {
            return;
        }
        let Some(config) = self.selected_config() else {
            return;
        };
        let Some(acl) = self.selected_acl.clone() else {
            return;
        };
        let description = format_acl_confirmation("删除", &acl);
        let view = cx.entity();
        ramag_ui::open_confirm(
            "删除 Kafka ACL？",
            description,
            "删除",
            true,
            move |window, app| {
                view.update(app, |this, cx| {
                    this.execute_acl_operation(config, acl, false, window, cx);
                });
            },
            window,
            cx,
        );
    }

    fn execute_acl_operation(
        &mut self,
        config: KafkaClusterConfig,
        acl: KafkaAcl,
        create: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.acl_operation
            || !self.read_only.allows_admin()
            || self.selected_cluster_id.as_ref() != Some(&config.id)
        {
            return;
        }
        self.acl_operation_id = self.acl_operation_id.wrapping_add(1);
        let operation_id = self.acl_operation_id;
        let cluster_id = config.id.clone();
        let service = self.service.clone();
        self.acl_operation = true;
        self.notice = Some((
            if create {
                "正在创建 Kafka ACL…".into()
            } else {
                "正在删除 Kafka ACL…".into()
            },
            false,
        ));
        cx.spawn_in(window, async move |this, cx| {
            let result = if create {
                service.create_acl(&config, &acl).await
            } else {
                service.delete_acl(&config, &acl).await
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if this.acl_operation_id != operation_id
                    || this.selected_cluster_id.as_ref() != Some(&cluster_id)
                {
                    return;
                }
                this.acl_operation = false;
                match result {
                    Ok(()) => {
                        this.selected_acl = create.then_some(acl.clone());
                        this.acls_loaded = false;
                        this.acl_error = None;
                        this.notice = Some((
                            if create {
                                "Kafka ACL 已创建，正在刷新列表".into()
                            } else {
                                "Kafka ACL 已删除，正在刷新列表".into()
                            },
                            false,
                        ));
                        this.load_acls(config, window, cx);
                    }
                    Err(error) => {
                        this.mark_runtime_failure(
                            if create {
                                "创建 Kafka ACL"
                            } else {
                                "删除 Kafka ACL"
                            },
                            &error,
                        );
                        this.notice = Some((
                            if create {
                                format!("创建 Kafka ACL 失败：{}", error.user_message())
                            } else {
                                format!("删除 Kafka ACL 失败：{}", error.user_message())
                            },
                            true,
                        ));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

fn format_acl_confirmation(action: &str, acl: &KafkaAcl) -> String {
    format!(
        "将{action}规则：Principal={}，Host={}，Resource={}/{}，Pattern={}，Operation={}，Permission={}。请求会直接发送到当前 Kafka 集群。",
        acl.principal,
        acl.host,
        acl.resource_type.label(),
        acl.resource_name,
        acl.pattern_type.label(),
        acl.operation.label(),
        acl.permission.label(),
    )
}
