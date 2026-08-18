//! GPUI 渲染测试：headless 在内存渲染 VcsView（含 diff session 态）。
//! 验证整条 diff 渲染管线不 panic；截图被 macOS 屏幕录制权限挡，本测试是可重复真机验证替代。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use gpui::{
    AppContext, Entity, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase,
    VisualTestContext, point, px, size,
};
use ramag_domain::entities::{
    Branch, BranchKind, Commit, ConnectionConfig, ConnectionId, DiffKind, DiffLine, DiffLineKind,
    FileChangeKind, FileDiff, FileStatus, Hunk, LogOptions, QueryRecord, QueryRecordId, RepoConfig,
    RepoId, WorkingTreeStatus,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{GitDriver, Storage};
use std::{path::Path, sync::Arc};

use super::super::helpers::{ActiveView, FileContentSnapshot, FileTab, FileTabSource, GroupKind};
use super::VcsView;
/// 空壳 GitDriver：render 是纯展示、不调 driver，方法只需可编译且不 panic
struct MockGit;

#[async_trait]
impl GitDriver for MockGit {
    fn name(&self) -> &'static str {
        "mock"
    }
    async fn open_repo(&self, _: &Path) -> Result<RepoConfig> {
        Err(DomainError::NotImplemented("mock".into()))
    }
    async fn close_repo(&self, _: &RepoId) -> Result<()> {
        Ok(())
    }
    async fn status(&self, _: &RepoId) -> Result<WorkingTreeStatus> {
        Err(DomainError::NotImplemented("mock".into()))
    }
    async fn list_branches(&self, _: &RepoId, _: BranchKind) -> Result<Vec<Branch>> {
        Ok(vec![])
    }
    async fn log(&self, _: &RepoId, _: LogOptions) -> Result<Vec<Commit>> {
        Ok(vec![])
    }
    async fn diff_file(&self, _: &RepoId, _: &str, _: DiffKind) -> Result<FileDiff> {
        Err(DomainError::NotImplemented("mock".into()))
    }
}

/// 空壳 Storage：render 不调 storage
struct MockStorage;

#[async_trait]
impl Storage for MockStorage {
    async fn list_connections(&self) -> Result<Vec<ConnectionConfig>> {
        Ok(vec![])
    }
    async fn get_connection(&self, _: &ConnectionId) -> Result<Option<ConnectionConfig>> {
        Ok(None)
    }
    async fn save_connection(&self, _: &ConnectionConfig) -> Result<()> {
        Ok(())
    }
    async fn delete_connection(&self, _: &ConnectionId) -> Result<()> {
        Ok(())
    }
    async fn append_history(&self, _: &QueryRecord) -> Result<()> {
        Ok(())
    }
    async fn list_history(&self, _: Option<&ConnectionId>, _: usize) -> Result<Vec<QueryRecord>> {
        Ok(vec![])
    }
    async fn delete_history(&self, _: &QueryRecordId) -> Result<()> {
        Ok(())
    }
    async fn clear_history(&self, _: Option<&ConnectionId>) -> Result<()> {
        Ok(())
    }
    async fn get_preference(&self, _: &str) -> Result<Option<String>> {
        Ok(None)
    }
    async fn set_preference(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
}

fn mock_repo() -> RepoConfig {
    RepoConfig {
        id: RepoId::new(),
        name: "test-repo".into(),
        path: "/tmp/test-repo".into(),
        last_opened_at: None,
    }
}

fn dline(kind: DiffLineKind, old: Option<u32>, new: Option<u32>, text: &str) -> DiffLine {
    DiffLine {
        kind,
        old_lineno: old,
        new_lineno: new,
        text: text.into(),
    }
}

/// 含 context + delete + add 的多行 diff（触发 split 双栏配对渲染）。
/// 用 `.rs` 路径 + 真 Rust 代码行，让语法高亮路径（SyntaxHighlighter）参与渲染验证。
fn test_diff() -> FileDiff {
    FileDiff {
        path: "a.rs".into(),
        old_path: None,
        change_kind: FileChangeKind::Modified,
        binary: false,
        old_mode: None,
        new_mode: None,
        hunks: vec![Hunk {
            old_start: 1,
            old_lines: 3,
            new_start: 1,
            new_lines: 3,
            heading: None,
            lines: vec![
                dline(DiffLineKind::Context, Some(1), Some(1), "fn main() {"),
                dline(DiffLineKind::Delete, Some(2), None, "    let x = 1;"),
                dline(DiffLineKind::Add, None, Some(2), "    let y = 2;"),
                dline(DiffLineKind::Context, Some(3), Some(3), "}"),
            ],
        }],
    }
}

fn scroll_test_diff() -> FileDiff {
    FileDiff {
        path: "wide.rs".into(),
        old_path: None,
        change_kind: FileChangeKind::Modified,
        binary: false,
        old_mode: None,
        new_mode: None,
        hunks: vec![Hunk {
            old_start: 1,
            old_lines: 120,
            new_start: 1,
            new_lines: 120,
            heading: None,
            lines: (0..120)
                .map(|index| DiffLine {
                    kind: DiffLineKind::Context,
                    old_lineno: Some(index + 1),
                    new_lineno: Some(index + 1),
                    text: format!("let line_{index} = \"{}\";", "x".repeat(400)),
                })
                .collect(),
        }],
    }
}

fn mock_status() -> WorkingTreeStatus {
    WorkingTreeStatus {
        head_branch: Some("main".into()),
        head_commit: Some("abc1234".into()),
        files: vec![FileStatus {
            path: "a.rs".into(),
            old_path: None,
            staged: None,
            unstaged: Some(FileChangeKind::Modified),
        }],
        ..Default::default()
    }
}

/// 注入「打开仓库 + 选中改动文件 + diff 已加载」的 Session 态
fn inject_diff_session(v: &mut VcsView) {
    let repo = mock_repo();
    v.open_repos = vec![repo.clone()];
    v.repo = Some(repo);
    v.active_view = ActiveView::Session;
    v.status = Some(mock_status());
    let diff = std::rc::Rc::new(test_diff());
    let syntax = std::rc::Rc::new(super::super::syntax::DiffSyntaxSnapshot::new(
        &diff,
        Some("rust"),
    ));
    v.current_diff = Some(diff.clone());
    v.current_diff_syntax = Some(syntax.clone());
    v.selected_file = Some(("a.rs".into(), GroupKind::Unstaged));
    v.file_tabs = vec![FileTab {
        path: "a.rs".into(),
        source: FileTabSource::Changes(GroupKind::Unstaged),
        cached_diff: Some(diff),
        cached_diff_syntax: Some(syntax),
        cached_content: None,
    }];
    v.active_file_tab_idx = Some(0);
}

fn inject_scroll_diff_session(v: &mut VcsView) {
    inject_diff_session(v);
    let diff = std::rc::Rc::new(scroll_test_diff());
    let syntax = std::rc::Rc::new(super::super::syntax::DiffSyntaxSnapshot::new(
        &diff,
        Some("rust"),
    ));
    v.current_diff = Some(diff.clone());
    v.current_diff_syntax = Some(syntax.clone());
    v.file_tabs[0].cached_diff = Some(diff);
    v.file_tabs[0].cached_diff_syntax = Some(syntax);
    v.diff_view_mode = super::super::helpers::DiffViewMode::FullFile;
}

/// 注入 Project Files 直接查看文件内容的 Session 态。
fn inject_file_content_session(v: &mut VcsView) {
    let repo = mock_repo();
    v.open_repos = vec![repo.clone()];
    v.repo = Some(repo);
    v.active_view = ActiveView::Session;

    let text = (0..200)
        .map(|index| format!("fn item_{index}() {{\tprintln!(\"{index}\"); }}"))
        .collect::<Vec<_>>()
        .join("\n");
    let snapshot = FileContentSnapshot {
        path: "src/generated.rs".into(),
        text: std::rc::Rc::new(text),
        line_count: 200,
        revision: 0,
        dirty: false,
        truncated: false,
        binary: false,
        error: None,
    };
    v.selected_pf_path = Some(snapshot.path.clone());
    v.current_file_content = Some(snapshot.clone());
    v.queue_project_editor_load(&snapshot);
    v.file_tabs = vec![FileTab {
        path: snapshot.path.clone(),
        source: FileTabSource::ProjectFiles,
        cached_diff: None,
        cached_diff_syntax: None,
        cached_content: Some(snapshot),
    }];
    v.active_file_tab_idx = Some(0);
}

/// 输入框绘制依赖 gpui-component 的窗口根节点，测试必须复刻生产环境的 Root 包装。
fn add_vcs_window(cx: &mut TestAppContext) -> (Entity<VcsView>, &mut VisualTestContext) {
    cx.update(gpui_component::init);

    let mut view = None;
    let (_, visual_cx) = cx.add_window_view(|window, cx| {
        let vcs_view =
            cx.new(|cx| VcsView::new(Arc::new(MockGit), Arc::new(MockStorage), window, cx));
        view = Some(vcs_view.clone());
        gpui_component::Root::new(vcs_view, window, cx)
    });

    (view.expect("VcsView should be initialized"), visual_cx)
}

/// 渲染整条 IDE 布局（含 diff split 5-list：左 gutter/content + 中间列 + 右 gutter/content + 行配对 + scroll）不 panic。
/// 能跑完 add_window_view（内部 draw）+ run_until_parked 即证明渲染管线健康。
#[gpui::test]
fn vcs_view_renders_diff_split_without_panic(cx: &mut TestAppContext) {
    let (view, cx) = add_vcs_window(cx);

    view.update(cx, |v, cx| {
        inject_diff_session(v);
        cx.notify();
    });
    cx.run_until_parked();

    view.read_with(cx, |v, _| {
        assert!(v.current_diff.is_some(), "diff 应已注入并参与渲染");
        assert_eq!(v.file_tabs.len(), 1, "应有 1 个文件 tab");
    });

    // 再渲染一帧（状态不变），验证幂等不崩
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
}

/// 分栏状态在 VcsView 更新期间发出通知时，不得反向重入更新同一个 VcsView。
#[gpui::test]
fn history_split_state_notification_does_not_reenter_vcs_view(cx: &mut TestAppContext) {
    let (view, cx) = add_vcs_window(cx);
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        view.ide_left_resize.update(cx, |_, cx| cx.notify());
        cx.notify();
    });
    cx.run_until_parked();
}

/// 上下区域共享同一左栏宽度时，分隔线必须落在同一横坐标。
#[gpui::test]
fn history_left_column_aligns_with_files_column(cx: &mut TestAppContext) {
    let (view, cx) = add_vcs_window(cx);
    view.update(cx, |view, cx| {
        inject_diff_session(view);
        view.history_pane_visible = true;
        cx.notify();
    });
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    cx.run_until_parked();

    let files = cx
        .debug_bounds("vcs-files-column")
        .expect("files column should be rendered");
    let history = cx
        .debug_bounds("vcs-history-left-column")
        .expect("history left column should be rendered");

    assert_eq!(files.right(), history.right());
}

/// 与数据库资源树保持相同的 180–600 px 可拖动范围。
#[gpui::test]
fn files_column_matches_database_tree_resize_range(cx: &mut TestAppContext) {
    let (view, cx) = add_vcs_window(cx);
    view.update(cx, |view, cx| {
        inject_diff_session(view);
        cx.notify();
    });
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    cx.run_until_parked();

    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.ide_left_resize.update(cx, |state, cx| {
                state.resize_panel(0, px(100.0), window, cx);
            });
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("vcs-files-column")
            .expect("文件栏应保持显示")
            .size
            .width,
        px(180.0)
    );

    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.ide_left_resize.update(cx, |state, cx| {
                state.resize_panel(0, px(700.0), window, cx);
            });
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("vcs-files-column")
            .expect("文件栏应保持显示")
            .size
            .width,
        px(600.0)
    );
}

#[gpui::test]
fn repository_resize_is_isolated_by_tab(cx: &mut TestAppContext) {
    let (view, cx) = add_vcs_window(cx);
    let first = mock_repo();
    let mut second = mock_repo();
    second.id = RepoId::new();
    second.name = "second-repo".into();
    second.path = "/tmp/second-repo".into();

    view.update(cx, |view, cx| {
        inject_diff_session(view);
        view.repo = Some(first.clone());
        view.open_repos = vec![first.clone(), second.clone()];
        cx.notify();
    });
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    cx.run_until_parked();

    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.ide_left_resize.update(cx, |state, cx| {
                state.resize_panel(0, px(360.0), window, cx);
            });
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("vcs-files-column")
            .expect("首个仓库应显示文件栏")
            .size
            .width,
        px(360.0)
    );

    view.update(cx, |view, cx| {
        view.save_current_session_to_cache(cx);
        view.repo = Some(second.clone());
        assert!(!view.restore_session_from_cache(&second.path, cx));
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("vcs-files-column")
            .expect("第二个仓库应显示文件栏")
            .size
            .width,
        px(280.0),
        "第二个仓库不应继承首个仓库拖动后的宽度"
    );

    view.update(cx, |view, cx| {
        view.save_current_session_to_cache(cx);
        view.repo = Some(first.clone());
        assert!(view.restore_session_from_cache(&first.path, cx));
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("vcs-files-column")
            .expect("切回首个仓库应显示文件栏")
            .size
            .width,
        px(360.0),
        "切回首个仓库应保留当前会话内自己的宽度"
    );
}

/// 关闭仓库标签会销毁其布局会话，再次打开不得恢复旧宽度。
#[gpui::test]
fn closed_repository_does_not_restore_resized_left_column(cx: &mut TestAppContext) {
    let (view, cx) = add_vcs_window(cx);
    let repo = mock_repo();
    view.update(cx, |view, cx| {
        inject_diff_session(view);
        view.repo = Some(repo.clone());
        view.open_repos = vec![repo.clone()];
        cx.notify();
    });
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    cx.run_until_parked();

    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.ide_left_resize.update(cx, |state, cx| {
                state.resize_panel(0, px(520.0), window, cx);
            });
        });
    });
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        view.remove_open_repo(repo.path.clone(), cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert!(!view.repo_session_cache.contains_key(&repo.path));
        assert!(!view.repo_session_order.contains(&repo.path));
    });

    view.update(cx, |view, cx| {
        view.repo = Some(repo.clone());
        view.open_repos = vec![repo.clone()];
        view.active_view = ActiveView::Session;
        assert!(!view.restore_session_from_cache(&repo.path, cx));
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds("vcs-files-column")
            .expect("重新打开后应显示文件栏")
            .size
            .width,
        px(280.0)
    );
}

/// 侧栏右键新建分支在 VcsView 更新结束后打开，不得再次借用同一个实体。
#[gpui::test]
fn sidebar_create_branch_dialog_opens_without_reentrant_update(cx: &mut TestAppContext) {
    let (view, cx) = add_vcs_window(cx);
    view.update(cx, |view, cx| {
        inject_diff_session(view);
        cx.notify();
    });
    cx.run_until_parked();

    cx.update(|window, app| {
        super::super::sidebar::open_sidebar_create_dialog(
            view.clone(),
            super::super::sidebar::SidebarSection::Local,
            window,
            app,
        );
    });
    cx.run_until_parked();
}

/// 横向查看长代码时，触控板附带的少量纵向位移不能带着 Diff 行上下移动。
#[gpui::test]
fn diff_diagonal_scroll_moves_only_horizontally(cx: &mut TestAppContext) {
    let (view, cx) = add_vcs_window(cx);
    view.update(cx, |view, cx| {
        inject_scroll_diff_session(view);
        cx.notify();
    });
    cx.run_until_parked();

    let position = cx
        .debug_bounds("vcs-diff-scroll-region")
        .expect("diff scroll region should be rendered")
        .center();
    cx.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(-80.0), px(-8.0))),
        touch_phase: TouchPhase::Moved,
        ..Default::default()
    });

    view.read_with(cx, |view, _| {
        let horizontal = view.diff_h_scroll.offset();
        let vertical = view.diff_scroll.0.borrow().base_handle.offset();
        assert!(horizontal.x < px(0.0), "横向手势应移动 Diff 内容");
        assert_eq!(vertical.y, px(0.0), "横向手势不应移动 Diff 行");
    });
}

/// 切到「全文件」diff 视图模式后仍能渲染（context_lines 路径）
#[gpui::test]
fn vcs_view_renders_full_file_diff_mode(cx: &mut TestAppContext) {
    let (view, cx) = add_vcs_window(cx);
    view.update(cx, |v, cx| {
        inject_diff_session(v);
        v.diff_view_mode = super::super::helpers::DiffViewMode::FullFile;
        cx.notify();
    });
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert!(matches!(
            v.diff_view_mode,
            super::super::helpers::DiffViewMode::FullFile
        ));
    });
}

/// Diff 全屏只改变布局，既有 Diff 快照仍可直接渲染。
#[gpui::test]
fn vcs_view_renders_diff_fullscreen_without_reloading(cx: &mut TestAppContext) {
    let (view, cx) = add_vcs_window(cx);
    view.update(cx, |v, cx| {
        inject_diff_session(v);
        v.diff_fullscreen = true;
        cx.notify();
    });
    cx.run_until_parked();

    view.read_with(cx, |v, _| {
        assert!(v.diff_fullscreen);
        assert!(v.current_diff.is_some());
        assert!(!v.loading_diff);
    });
}

/// Project Files 直接查看走原生 Code Editor，重复渲染不 panic。
#[gpui::test]
fn vcs_view_renders_project_file_content_without_panic(cx: &mut TestAppContext) {
    let (view, cx) = add_vcs_window(cx);
    view.update(cx, |v, cx| {
        inject_file_content_session(v);
        cx.notify();
    });
    cx.run_until_parked();

    view.read_with(cx, |v, _| {
        assert_eq!(
            v.current_file_content
                .as_ref()
                .map(|snapshot| snapshot.line_count),
            Some(200)
        );
        assert_eq!(v.pf_editor_loaded_path.as_deref(), Some("src/generated.rs"));
    });

    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();

    // 未修改标签不显示圆点；切成 dirty 后覆盖圆点渲染分支，二者都必须稳定绘制。
    view.update(cx, |v, cx| {
        assert!(
            v.file_tabs[0]
                .cached_content
                .as_mut()
                .is_some_and(|snapshot| {
                    snapshot.dirty = true;
                    true
                })
        );
        v.pf_editor_dirty = true;
        cx.notify();
    });
    cx.run_until_parked();
    view.read_with(cx, |v, _| assert!(v.file_tabs[0].is_dirty()));
}
