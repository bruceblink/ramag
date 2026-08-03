//! JumpServer 资源详情、连接测试与导入。

use gpui::Context;

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
                            .find(|account| account.usable_for_direct_login())
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
            detail
                .accounts
                .iter()
                .any(|account| account.id == account_id && account.usable_for_direct_login())
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
