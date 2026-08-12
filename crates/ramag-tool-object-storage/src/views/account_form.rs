use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, EventEmitter, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};
use ramag_app::{ObjectStorageService, SavedObjectStorageAccount};
use ramag_domain::entities::{
    CloudProvider, MAX_OBJECT_STORAGE_ACCESS_KEY_ID_BYTES,
    MAX_OBJECT_STORAGE_ACCESS_KEY_SECRET_BYTES, MAX_OBJECT_STORAGE_ACCOUNT_NAME_BYTES,
    MAX_OBJECT_STORAGE_BUCKET_NAME_BYTES, MAX_OBJECT_STORAGE_KEY_BYTES,
    MAX_OBJECT_STORAGE_REGION_BYTES, ManualBucket, ObjectStorageAccount, SecretString,
};

mod render;

#[derive(Debug, Clone)]
pub(super) enum AccountFormEvent {
    Saved(Box<SavedObjectStorageAccount>),
    Cancelled,
}

impl EventEmitter<AccountFormEvent> for AccountFormPanel {}

#[derive(PartialEq)]
struct FormSnapshot {
    provider: CloudProvider,
    production: bool,
    values: Vec<String>,
    manual_buckets: Vec<ManualBucket>,
}

pub(super) struct AccountFormPanel {
    service: Arc<ObjectStorageService>,
    editing: Option<ObjectStorageAccount>,
    provider: CloudProvider,
    production: bool,
    name: Entity<InputState>,
    access_key_id: Entity<InputState>,
    access_key_secret: Entity<InputState>,
    bucket: Entity<InputState>,
    region: Entity<InputState>,
    root_prefix: Entity<InputState>,
    manual_buckets: Vec<ManualBucket>,
    saving: bool,
    feedback: Option<(String, bool)>,
    initial: FormSnapshot,
    _subscriptions: Vec<Subscription>,
}

impl AccountFormPanel {
    pub(super) fn new(
        service: Arc<ObjectStorageService>,
        account: Option<ObjectStorageAccount>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let provider = account
            .as_ref()
            .map_or(CloudProvider::TencentCos, |account| account.provider);
        let production = account.as_ref().is_some_and(|account| account.read_only);
        let name = input(
            MAX_OBJECT_STORAGE_ACCOUNT_NAME_BYTES,
            "例如：生产日志",
            false,
            account
                .as_ref()
                .map(|account| account.name.as_str())
                .unwrap_or_default(),
            window,
            cx,
        );
        let (access_key_id_placeholder, access_key_secret_placeholder) =
            credential_placeholders(provider, account.is_some());
        let access_key_id = input(
            MAX_OBJECT_STORAGE_ACCESS_KEY_ID_BYTES,
            access_key_id_placeholder,
            true,
            "",
            window,
            cx,
        );
        let access_key_secret = input(
            MAX_OBJECT_STORAGE_ACCESS_KEY_SECRET_BYTES,
            access_key_secret_placeholder,
            true,
            "",
            window,
            cx,
        );
        let bucket = input(
            MAX_OBJECT_STORAGE_BUCKET_NAME_BYTES,
            "Bucket 名称",
            false,
            "",
            window,
            cx,
        );
        let region = input(
            MAX_OBJECT_STORAGE_REGION_BYTES,
            region_placeholder(provider),
            false,
            default_region(provider),
            window,
            cx,
        );
        let root_prefix = input(
            MAX_OBJECT_STORAGE_KEY_BYTES,
            "Root Prefix（可选）",
            false,
            "",
            window,
            cx,
        );
        let manual_buckets = account
            .as_ref()
            .map(|account| account.manual_buckets.clone())
            .unwrap_or_default();
        let values = input_values(
            [
                &name,
                &access_key_id,
                &access_key_secret,
                &bucket,
                &region,
                &root_prefix,
            ],
            cx,
        );
        let initial = FormSnapshot {
            provider,
            production,
            values,
            manual_buckets: manual_buckets.clone(),
        };
        let mut subscriptions = Vec::new();
        for field in [
            &name,
            &access_key_id,
            &access_key_secret,
            &bucket,
            &region,
            &root_prefix,
        ] {
            subscriptions.push(cx.subscribe(field, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.feedback = None;
                    cx.notify();
                }
            }));
        }
        Self {
            service,
            editing: account,
            provider,
            production,
            name,
            access_key_id,
            access_key_secret,
            bucket,
            region,
            root_prefix,
            manual_buckets,
            saving: false,
            feedback: None,
            initial,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn title(&self) -> &'static str {
        if self.editing.is_some() {
            "编辑云存储账号"
        } else {
            "新建云存储账号"
        }
    }

    pub(super) fn is_saving(&self) -> bool {
        self.saving
    }

    pub(super) fn is_dirty(&self, cx: &gpui::App) -> bool {
        self.snapshot(cx) != self.initial
    }

    #[cfg(test)]
    pub(super) fn production_enabled(&self) -> bool {
        self.production
    }

    #[cfg(test)]
    pub(super) fn region_value(&self, cx: &gpui::App) -> String {
        value(&self.region, cx)
    }

    fn snapshot(&self, cx: &gpui::App) -> FormSnapshot {
        FormSnapshot {
            provider: self.provider,
            production: self.production,
            values: input_values(
                [
                    &self.name,
                    &self.access_key_id,
                    &self.access_key_secret,
                    &self.bucket,
                    &self.region,
                    &self.root_prefix,
                ],
                cx,
            ),
            manual_buckets: self.manual_buckets.clone(),
        }
    }

    fn set_provider(
        &mut self,
        provider: CloudProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.saving && self.provider != provider {
            let previous_default_region = default_region(self.provider);
            let current_region = value(&self.region, cx);
            self.provider = provider;
            let (access_key_id, access_key_secret) =
                credential_placeholders(provider, self.editing.is_some());
            self.access_key_id.update(cx, |state, cx| {
                state.set_placeholder(access_key_id, window, cx);
            });
            self.access_key_secret.update(cx, |state, cx| {
                state.set_placeholder(access_key_secret, window, cx);
            });
            self.region.update(cx, |state, cx| {
                state.set_placeholder(region_placeholder(provider), window, cx);
                if current_region.is_empty() || current_region == previous_default_region {
                    state.set_value(default_region(provider), window, cx);
                }
            });
            self.feedback = None;
            cx.notify();
        }
    }

    fn add_manual_bucket(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let mut bucket = ManualBucket::new(value(&self.bucket, cx), value(&self.region, cx));
        let root_prefix = value(&self.root_prefix, cx);
        bucket.root_prefix = (!root_prefix.is_empty()).then_some(root_prefix);
        match bucket.validate_for_provider(self.provider) {
            Ok(()) => {
                self.manual_buckets.push(bucket);
                for field in [&self.bucket, &self.root_prefix] {
                    field.update(cx, |state, cx| state.set_value("", window, cx));
                }
                let region = default_region(self.provider);
                self.region.update(cx, |state, cx| {
                    state.set_value(region, window, cx);
                });
                self.feedback = Some(("Bucket 已添加，保存后生效".into(), false));
            }
            Err(error) => self.feedback = Some((error, true)),
        }
        cx.notify();
    }

    fn handle_save(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let mut account = self
            .editing
            .clone()
            .unwrap_or_else(|| ObjectStorageAccount::new(value(&self.name, cx), self.provider));
        account.name = value(&self.name, cx);
        account.provider = self.provider;
        account.read_only = self.production;
        account.manual_buckets = self.manual_buckets.clone();
        if account.manual_buckets.is_empty() {
            self.feedback = Some(("请至少添加一个 Bucket".into(), true));
            cx.notify();
            return;
        }
        let access_key_id = value(&self.access_key_id, cx);
        let access_key_secret = value(&self.access_key_secret, cx);
        if !access_key_id.is_empty() {
            account.access_key_id = SecretString::new(access_key_id);
        }
        if !access_key_secret.is_empty() {
            account.access_key_secret = SecretString::new(access_key_secret);
        }
        if let Err(error) = account.validate() {
            self.feedback = Some((error, true));
            cx.notify();
            return;
        }
        self.saving = true;
        self.feedback = Some(("正在验证并保存…".into(), false));
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = service.save_account(account).await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(saved) => cx.emit(AccountFormEvent::Saved(Box::new(saved))),
                    Err(error) => {
                        tracing::error!(operation = "object_storage_account_save", error = %error, "save object storage account failed");
                        this.feedback = Some((format!("保存失败：{}", error.user_message()), true));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn handle_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        if !self.is_dirty(cx) {
            cx.emit(AccountFormEvent::Cancelled);
            return;
        }
        let form = cx.entity();
        ramag_ui::open_confirm(
            "放弃修改？",
            "表单有未保存的修改，关闭将丢弃这些修改。",
            "放弃修改",
            true,
            move |_, app| {
                form.update(app, |_this, cx| cx.emit(AccountFormEvent::Cancelled));
            },
            window,
            cx,
        );
    }
}

fn input(
    max_bytes: usize,
    placeholder: &'static str,
    masked: bool,
    default_value: &str,
    window: &mut Window,
    cx: &mut Context<AccountFormPanel>,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .validate(move |value, _| value.len() <= max_bytes)
            .placeholder(placeholder)
            .masked(masked)
            .default_value(default_value.to_string())
    })
}

fn input_values<'a>(
    fields: impl IntoIterator<Item = &'a Entity<InputState>>,
    cx: &gpui::App,
) -> Vec<String> {
    fields
        .into_iter()
        .map(|field| field.read(cx).value().to_string())
        .collect()
}

fn value(field: &Entity<InputState>, cx: &gpui::App) -> String {
    field.read(cx).value().trim().to_string()
}

fn credential_placeholders(provider: CloudProvider, editing: bool) -> (&'static str, &'static str) {
    match (provider, editing) {
        (CloudProvider::TencentCos, false) => ("请输入 SecretId", "请输入 SecretKey"),
        (CloudProvider::TencentCos, true) => ("********", "********"),
        (CloudProvider::AliyunOss, false) => ("请输入 AccessKey ID", "请输入 AccessKey Secret"),
        (CloudProvider::AliyunOss, true) => ("********", "********"),
    }
}

fn default_region(provider: CloudProvider) -> &'static str {
    match provider {
        CloudProvider::TencentCos => "ap-shanghai",
        CloudProvider::AliyunOss => "cn-shanghai",
    }
}

fn region_placeholder(provider: CloudProvider) -> &'static str {
    match provider {
        CloudProvider::TencentCos => "Region，例如 ap-shanghai",
        CloudProvider::AliyunOss => "Region，例如 cn-shanghai",
    }
}

#[cfg(test)]
mod tests {
    use super::{CloudProvider, credential_placeholders};

    #[test]
    fn editing_credentials_show_safe_mask_placeholders() {
        assert_eq!(
            credential_placeholders(CloudProvider::TencentCos, true),
            ("********", "********")
        );
        assert_eq!(
            credential_placeholders(CloudProvider::AliyunOss, true),
            ("********", "********")
        );
    }
}
