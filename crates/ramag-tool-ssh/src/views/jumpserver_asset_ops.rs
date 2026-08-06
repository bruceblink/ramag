//! JumpServer 资源详情、连接测试与导入。

use gpui::Context;
use ramag_domain::entities::JumpServerRdpSession;

use super::jumpserver_dialog::{
    JumpServerEvent, JumpServerOperation, JumpServerPanel, detail_unavailable_message,
};

impl JumpServerPanel {
    pub(super) fn select_asset(&mut self, asset_id: String, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        if self.selected_asset_id.as_deref() == Some(asset_id.as_str()) {
            self.generation = self.generation.wrapping_add(1);
            self.selected_asset_id = None;
            self.detail = None;
            self.detail_error = None;
            self.selected_account_id = None;
            cx.notify();
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        let Some(asset) = self
            .assets
            .iter()
            .find(|asset| asset.id == asset_id && asset.active)
            .cloned()
        else {
            return;
        };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.selected_asset_id = Some(asset.id.clone());
        self.detail = None;
        self.detail_error = None;
        self.selected_account_id = None;
        self.operation = Some(JumpServerOperation::LoadingDetail);
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = service.jumpserver_asset_detail(&session, &asset).await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.operation = None;
                match result {
                    Ok(detail) => {
                        this.selected_account_id = detail
                            .accounts
                            .iter()
                            .find(|account| {
                                (detail.ssh_enabled && account.usable_for_direct_login())
                                    || (detail.rdp_web_enabled && account.usable_for_web_session())
                            })
                            .map(|account| account.id.clone());
                        this.detail_error = detail_unavailable_message(&detail);
                        this.detail = Some(detail);
                    }
                    Err(error) => {
                        this.detail_error =
                            Some(format!("读取该资源的授权账号失败：{}", error.message()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn select_account(&mut self, account_id: String, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        let usable = self.detail.as_ref().is_some_and(|detail| {
            detail.accounts.iter().any(|account| {
                account.id == account_id
                    && ((detail.ssh_enabled && account.usable_for_direct_login())
                        || (detail.rdp_web_enabled && account.usable_for_web_session()))
            })
        });
        if usable {
            self.selected_account_id = Some(account_id);
            cx.notify();
        }
    }

    pub(super) fn test_selected(&mut self, cx: &mut Context<Self>) {
        self.run_selected_operation(JumpServerOperation::Testing, cx);
    }

    pub(super) fn save_selected(&mut self, cx: &mut Context<Self>) {
        self.run_selected_operation(JumpServerOperation::Saving, cx);
    }

    pub(super) fn open_selected_rdp(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        let (Some(session), Some(connection_id), Some(asset_id), Some(account_id)) = (
            self.session.clone(),
            self.selected_connection_id.clone(),
            self.selected_asset_id.clone(),
            self.selected_account_id.clone(),
        ) else {
            self.notify_error("请先选择资源和资产账号");
            cx.notify();
            return;
        };
        let Some(account) = self.detail.as_ref().and_then(|detail| {
            detail
                .accounts
                .iter()
                .find(|account| account.id == account_id)
                .cloned()
        }) else {
            self.notify_error("选中的授权账号已失效，请重新选择");
            cx.notify();
            return;
        };
        let Some(asset) = self
            .assets
            .iter()
            .find(|asset| asset.id == asset_id)
            .cloned()
        else {
            self.notify_error("选中的资源已不存在，请刷新后重试");
            cx.notify();
            return;
        };
        let record = match JumpServerRdpSession::from_selection(
            connection_id,
            session.base_url.clone(),
            &asset,
            &account,
        ) {
            Ok(record) => record,
            Err(error) => {
                self.notify_error(format!("远程会话信息无效：{error}"));
                cx.notify();
                return;
            }
        };

        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.operation = Some(JumpServerOperation::OpeningRdp);
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = service
                .create_jumpserver_rdp_web_session(&session, &asset, &account_id)
                .await;
            let result = match result {
                Ok(url) => {
                    let history_error = service.record_jumpserver_rdp_session(record).await.err();
                    Ok((url, history_error))
                }
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.operation = None;
                match result {
                    Ok((url, None)) => {
                        cx.open_url(&url);
                        this.notify_success("已在浏览器中打开远程桌面");
                    }
                    Ok((url, Some(error))) => {
                        cx.open_url(&url);
                        this.notify_error(format!(
                            "远程桌面已打开，但保存最近会话失败：{}",
                            error.message()
                        ));
                    }
                    Err(error) => {
                        this.notify_error(format!("打开远程桌面失败：{}", error.message()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn run_selected_operation(&mut self, operation: JumpServerOperation, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        let (Some(session), Some(asset_id), Some(account_id)) = (
            self.session.clone(),
            self.selected_asset_id.clone(),
            self.selected_account_id.clone(),
        ) else {
            self.notify_error("请先选择资源和资产账号");
            cx.notify();
            return;
        };
        let Some(asset) = self
            .assets
            .iter()
            .find(|asset| asset.id == asset_id)
            .cloned()
        else {
            return;
        };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.operation = Some(operation);
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = if operation == JumpServerOperation::Testing {
                service
                    .test_jumpserver_asset(&session, &asset, &account_id)
                    .await
            } else {
                service
                    .save_jumpserver_asset(&session, &asset, &account_id)
                    .await
            };
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.operation = None;
                match result {
                    Ok(profile) if operation == JumpServerOperation::Saving => {
                        this.saved_selections
                            .insert((asset.id.clone(), account_id.clone()));
                        this.notify_success(format!("已导入连接：{}", profile.name));
                        cx.emit(JumpServerEvent::Saved(Box::new(profile)));
                    }
                    Ok(_) => {
                        this.notify_success("连接成功");
                    }
                    Err(error) => {
                        let action = if operation == JumpServerOperation::Testing {
                            "测试"
                        } else {
                            "保存"
                        };
                        this.notify_error(format!("{action}失败：{}", error.message()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
