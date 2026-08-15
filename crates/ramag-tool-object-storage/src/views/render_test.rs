#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use async_trait::async_trait;
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use gpui_component::WindowExt as _;
use ramag_app::ObjectStorageService;
use ramag_domain::entities::{
    CloudProvider, ConnectionConfig, ConnectionId, HttpsEndpoint, ObjectCapabilities,
    ObjectDownloadRequest, ObjectEntry, ObjectEntryKind, ObjectListCursor, ObjectListQuery,
    ObjectMetadata, ObjectPage, ObjectStorageAccount, ObjectStorageAccountId,
    ObjectStorageAccountSnapshot, ObjectStorageMount, ObjectStorageMountId, ObjectTextPreview,
    ObjectUploadRequest, QueryRecord, QueryRecordId, TransferCancellation,
};
use ramag_domain::error::{ObjectStorageResult, Result};
use ramag_domain::traits::{ObjectStorageDriver, Storage};

use super::ObjectStorageView;
use super::account_form::AccountFormPanel;
use super::model::{ObjectTransferDirection, TransferUi};

struct TestObjectStorage;

struct TestStorage;

#[async_trait]
impl Storage for TestStorage {
    async fn list_connections(&self) -> Result<Vec<ConnectionConfig>> {
        Ok(Vec::new())
    }

    async fn get_connection(&self, _id: &ConnectionId) -> Result<Option<ConnectionConfig>> {
        Ok(None)
    }

    async fn save_connection(&self, _config: &ConnectionConfig) -> Result<()> {
        Ok(())
    }

    async fn delete_connection(&self, _id: &ConnectionId) -> Result<()> {
        Ok(())
    }

    async fn list_object_storage_accounts(&self) -> Result<Vec<ObjectStorageAccount>> {
        Ok(Vec::new())
    }

    async fn append_history(&self, _record: &QueryRecord) -> Result<()> {
        Ok(())
    }

    async fn list_history(
        &self,
        _connection_id: Option<&ConnectionId>,
        _limit: usize,
    ) -> Result<Vec<QueryRecord>> {
        Ok(Vec::new())
    }

    async fn delete_history(&self, _id: &QueryRecordId) -> Result<()> {
        Ok(())
    }

    async fn clear_history(&self, _connection_id: Option<&ConnectionId>) -> Result<()> {
        Ok(())
    }

    async fn get_preference(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }

    async fn set_preference(&self, _key: &str, _value: &str) -> Result<()> {
        Ok(())
    }

    async fn seal(&self, plain: &[u8]) -> Result<Vec<u8>> {
        Ok(plain.to_vec())
    }

    async fn unseal(&self, cipher: &[u8]) -> Result<Vec<u8>> {
        Ok(cipher.to_vec())
    }
}

#[async_trait]
impl ObjectStorageDriver for TestObjectStorage {
    async fn capabilities(
        &self,
        _account: &ObjectStorageAccountSnapshot,
        _mount: &ObjectStorageMount,
    ) -> ObjectStorageResult<ObjectCapabilities> {
        Ok(ObjectCapabilities {
            stat: true,
            read: true,
            write: true,
            delete: true,
            list: true,
            atomic_create: true,
        })
    }

    async fn list_page(
        &self,
        _account: &ObjectStorageAccountSnapshot,
        _mount: &ObjectStorageMount,
        _query: &ObjectListQuery,
        _cursor: Option<&ObjectListCursor>,
        _request_generation: u64,
    ) -> ObjectStorageResult<ObjectPage> {
        Ok(ObjectPage {
            entries: Vec::new(),
            next_cursor: None,
            capped: false,
        })
    }

    async fn stat(
        &self,
        _account: &ObjectStorageAccountSnapshot,
        _mount: &ObjectStorageMount,
        key: &str,
    ) -> ObjectStorageResult<ObjectMetadata> {
        Ok(ObjectMetadata {
            key: key.into(),
            size: 0,
            last_modified: None,
            etag: None,
            version: None,
            content_type: None,
            user_metadata: Vec::new(),
            storage_class: None,
        })
    }

    async fn read_text_preview(
        &self,
        _account: &ObjectStorageAccountSnapshot,
        _mount: &ObjectStorageMount,
        _key: &str,
    ) -> ObjectStorageResult<ObjectTextPreview> {
        Ok(ObjectTextPreview {
            content: "preview".into(),
            total_bytes: 7,
            truncated: false,
        })
    }

    async fn upload(&self, _request: ObjectUploadRequest) -> ObjectStorageResult<()> {
        Ok(())
    }

    async fn download(&self, _request: ObjectDownloadRequest) -> ObjectStorageResult<()> {
        Ok(())
    }

    async fn delete(
        &self,
        _account: &ObjectStorageAccountSnapshot,
        _mount: &ObjectStorageMount,
        _key: &str,
    ) -> ObjectStorageResult<()> {
        Ok(())
    }

    async fn invalidate_account(
        &self,
        _account_id: &ObjectStorageAccountId,
        _minimum_revision: u64,
    ) -> ObjectStorageResult<()> {
        Ok(())
    }

    async fn shutdown(&self) -> ObjectStorageResult<()> {
        Ok(())
    }
}

fn service() -> Arc<ObjectStorageService> {
    let infra = Arc::new(TestObjectStorage);
    Arc::new(ObjectStorageService::new(infra, Arc::new(TestStorage)))
}

fn add_form_window(
    cx: &mut TestAppContext,
    service: Arc<ObjectStorageService>,
) -> (Entity<AccountFormPanel>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    let mut form = None;
    let (_, visual_cx) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| AccountFormPanel::new(service, None, window, cx));
        form = Some(entity.clone());
        gpui_component::Root::new(entity, window, cx)
    });
    (form.expect("account form should be initialized"), visual_cx)
}

fn add_workspace_window(
    cx: &mut TestAppContext,
    service: Arc<ObjectStorageService>,
) -> (Entity<ObjectStorageView>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    let mut view = None;
    let (_, visual_cx) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| ObjectStorageView::new(service, window, cx));
        view = Some(entity.clone());
        gpui_component::Root::new(entity, window, cx)
    });
    (
        view.expect("object storage view should be initialized"),
        visual_cx,
    )
}

#[gpui::test]
fn provider_cards_are_the_first_equal_width_form_row(cx: &mut TestAppContext) {
    let (form, cx) = add_form_window(cx, service());
    cx.simulate_resize(size(px(720.0), px(800.0)));
    cx.run_until_parked();

    let cos = cx
        .debug_bounds("object-provider-tencent-cos")
        .expect("COS provider card should be rendered");
    let oss = cx
        .debug_bounds("object-provider-aliyun-oss")
        .expect("OSS provider card should be rendered");
    let name = cx
        .debug_bounds("object-account-name-field")
        .expect("account name should be rendered");
    let bucket = cx
        .debug_bounds("object-manual-bucket-field")
        .expect("manual bucket field should be rendered");
    let add = cx
        .debug_bounds("add-manual-bucket-layout")
        .expect("manual bucket add button should be rendered");

    assert_eq!(cos.origin.y, oss.origin.y);
    assert_eq!(cos.size.width, oss.size.width);
    assert!(cos.origin.x < oss.origin.x);
    assert!(cos.origin.y < name.origin.y, "服务商选择必须位于表单第一行");
    assert_eq!(
        bucket.origin.y + bucket.size.height,
        add.origin.y + add.size.height,
        "添加按钮必须与输入框底部对齐"
    );
    assert!(
        add.size.height < bucket.size.height,
        "添加按钮不能被字段标签所在行拉伸"
    );
    form.read_with(cx, |form, _| {
        assert!(!form.production_enabled(), "新账号的生产模式必须默认关闭");
    });
    form.read_with(cx, |form, cx| {
        assert_eq!(form.region_value(cx), "ap-shanghai");
    });

    cx.simulate_click(oss.center(), Modifiers::default());
    cx.run_until_parked();
    form.read_with(cx, |form, cx| {
        assert_eq!(form.region_value(cx), "cn-shanghai");
        assert!(!form.production_enabled());
    });
}

#[gpui::test]
fn account_manager_uses_provider_brand_icons(cx: &mut TestAppContext) {
    let (view, cx) = add_workspace_window(cx, service());
    cx.run_until_parked();
    view.update(cx, |view, cx| {
        view.accounts = Arc::new(vec![
            ObjectStorageAccount::new("cos", CloudProvider::TencentCos),
            ObjectStorageAccount::new("oss", CloudProvider::AliyunOss),
        ]);
        view.loading = false;
        view.management_visible = true;
        cx.notify();
    });
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("object-account-provider-icon-0").is_some(),
        "COS 账号应显示服务商图标"
    );
    assert!(
        cx.debug_bounds("object-account-provider-icon-1").is_some(),
        "OSS 账号应显示服务商图标"
    );
}

#[gpui::test]
fn object_workspace_matches_the_shared_compact_file_browser(cx: &mut TestAppContext) {
    let (view, cx) = add_workspace_window(cx, service());
    cx.run_until_parked();

    let account = ObjectStorageAccount::new("production", CloudProvider::AliyunOss);
    let mount = ObjectStorageMount {
        id: ObjectStorageMountId::new(),
        account_id: account.id.clone(),
        bucket: "logs-bucket".into(),
        region: "cn-hangzhou".into(),
        endpoint: HttpsEndpoint::parse_official(
            CloudProvider::AliyunOss,
            "https://oss-cn-hangzhou.aliyuncs.com",
        )
        .expect("official endpoint should be valid"),
        root_prefix: None,
        created_at: None,
        storage_class: None,
    };
    view.update(cx, |view, cx| {
        view.accounts = Arc::new(vec![account.clone()]);
        view.selected_account_id = Some(account.id.clone());
        view.management_visible = false;
        view.mounts = Arc::new(vec![mount.clone()]);
        view.selected_mount = Some(mount);
        view.show_mounts = true;
        view.loading = false;
        view.entries = Arc::new(vec![
            ObjectEntry {
                key: "logs/".into(),
                display_name: "logs".into(),
                kind: ObjectEntryKind::Prefix,
                operable: true,
                size: Some(0),
                last_modified: None,
                etag: None,
                content_type: None,
                storage_class: None,
            },
            ObjectEntry {
                key: "readme.txt".into(),
                display_name: "readme.txt".into(),
                kind: ObjectEntryKind::Object,
                operable: true,
                size: Some(1536),
                last_modified: None,
                etag: None,
                content_type: Some("text/plain".into()),
                storage_class: None,
            },
        ]);
        cx.notify();
    });
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    cx.run_until_parked();

    view.read_with(cx, |view, _| {
        assert!(!view.management_visible, "测试工作区不应被异步账号加载覆盖");
        assert_eq!(view.entries.len(), 2);
    });

    let path = cx
        .debug_bounds("object-directory-path")
        .expect("path row should be rendered");
    let toolbar = cx
        .debug_bounds("object-directory-toolbar")
        .expect("toolbar should be rendered");
    let directory = cx
        .debug_bounds("object-entry-logs/")
        .expect("directory row should be rendered");
    let columns = cx
        .debug_bounds("object-directory-columns")
        .expect("object columns should be rendered");
    let summary = cx
        .debug_bounds("object-directory-summary")
        .expect("summary should be rendered");

    assert_eq!(directory.size.height, px(28.0));
    assert!(path.origin.y < toolbar.origin.y);
    assert!(toolbar.origin.y < columns.origin.y);
    assert!(columns.origin.y < directory.origin.y);
    assert!(directory.origin.y < summary.origin.y);
    assert!(
        cx.debug_bounds("object-path-part-0").is_some(),
        "根路径应渲染为可点击面包屑"
    );
    assert!(
        cx.debug_bounds("object-favorite").is_none(),
        "目录工具栏不应保留收藏按钮"
    );
    assert!(
        cx.debug_bounds("object-toggle-detail").is_none(),
        "目录工具栏不应保留详情按钮"
    );

    let path_label = cx
        .debug_bounds("object-directory-path-label")
        .expect("path label should open direct path dialog");
    cx.simulate_click(path_label.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.update(|window, cx| window.has_active_dialog(cx)),
        "点击路径标签应打开可输入路径和管理收藏的窗口"
    );
}

#[gpui::test]
fn object_detail_keeps_metadata_and_removes_content_preview(cx: &mut TestAppContext) {
    let (view, cx) = add_workspace_window(cx, service());
    cx.run_until_parked();
    view.update(cx, |view, cx| {
        view.loading = false;
        view.management_visible = false;
        view.show_detail = true;
        view.selected_key = Some("config.json".into());
        view.detail_metadata = Some(ObjectMetadata {
            key: "config.json".into(),
            size: 7,
            last_modified: None,
            etag: Some("etag".into()),
            version: None,
            content_type: Some("application/json".into()),
            user_metadata: Vec::new(),
            storage_class: None,
        });
        cx.notify();
    });
    cx.simulate_resize(size(px(1200.0), px(700.0)));
    cx.run_until_parked();

    assert!(cx.debug_bounds("object-detail-header").is_some());
    assert!(cx.debug_bounds("object-detail-scroll").is_some());
    let panel = cx
        .debug_bounds("object-detail-panel")
        .expect("detail panel should be rendered");
    assert_eq!(panel.size.width, px(420.0));
    assert!(cx.debug_bounds("object-preview-content").is_none());
    assert!(cx.debug_bounds("object-preview-scroll").is_none());
}

#[gpui::test]
fn active_transfer_opens_bounded_progress_panel(cx: &mut TestAppContext) {
    let (view, cx) = add_workspace_window(cx, service());
    cx.run_until_parked();
    view.update(cx, |view, cx| {
        view.loading = false;
        view.management_visible = false;
        view.transfers_visible = true;
        view.transfers = vec![TransferUi {
            id: 1,
            account_id: ObjectStorageAccountId::new(),
            mount_id: ObjectStorageMountId::new(),
            key: "reports/result.zip".into(),
            label: "reports/result.zip".into(),
            local_path: "/tmp/result.zip".into(),
            direction: ObjectTransferDirection::Download,
            cancellation: TransferCancellation::default(),
            transferred: Arc::new(AtomicU64::new(512)),
            total: Arc::new(AtomicU64::new(1024)),
        }];
        cx.notify();
    });
    cx.simulate_resize(size(px(1200.0), px(700.0)));
    cx.run_until_parked();

    let panel = cx
        .debug_bounds("object-transfer-panel")
        .expect("active transfer should open progress panel");
    assert!(cx.debug_bounds("object-transfers").is_some());
    assert!(panel.size.width <= px(520.0));
    assert!(panel.origin.x > px(500.0));
}
