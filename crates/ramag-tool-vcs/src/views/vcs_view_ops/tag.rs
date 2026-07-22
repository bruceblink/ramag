//! VcsView Tag 异步操作：加载列表 + 创建 / 删除 / 推送

use gpui::Context;
use tracing::{error, info};

use super::super::helpers::{TagOp, default_remote_name};
use super::super::vcs_view::VcsView;

impl VcsView {
    /// 异步加载 tag 列表（仓库打开时 + tag 操作完成后调用）
    pub(in crate::views) fn reload_tags(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        self.loading_tags = true;
        self.tag_request_seq = self.tag_request_seq.wrapping_add(1);
        let request_seq = self.tag_request_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = driver.list_tags(&repo).await;
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo) || this.tag_request_seq != request_seq {
                    return;
                }
                this.loading_tags = false;
                match result {
                    Ok(list) => this.tags = list,
                    Err(e) => {
                        error!(error = %e, "load tags failed");
                        this.error = Some(format!("加载 Tag 列表失败：{e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Tag 操作：创建 / 删除 / 推送
    pub(in crate::views) fn run_tag_op(&mut self, op: TagOp, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        let push_remote = if matches!(op, TagOp::Push(_)) {
            match default_remote_name(&self.remotes) {
                Ok(remote) => Some(remote),
                Err(message) => {
                    self.error = Some(message);
                    cx.notify();
                    return;
                }
            }
        } else {
            None
        };
        if !self.begin_op("Tag 操作中…", cx) {
            return;
        }
        self.tag_request_seq = self.tag_request_seq.wrapping_add(1);
        self.loading_tags = false;

        cx.spawn(async move |this, cx| {
            let result = match &op {
                TagOp::Create { name, message } => {
                    // sign 暂未在 UI 暴露——release tag 通常不签名；future 加 toggle
                    driver
                        .create_tag(&repo, name, None, message.as_deref(), false)
                        .await
                }
                TagOp::Delete(name) => driver.delete_tag(&repo, name).await,
                TagOp::Push(name) => {
                    driver
                        .push_tag(&repo, push_remote.as_deref().unwrap_or("origin"), name)
                        .await
                }
            };
            let new_tags = driver.list_tags(&repo).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.busy_label = None;
                if !this.is_current_repo(&repo) {
                    cx.notify();
                    return;
                }
                if let Ok(tags) = &new_tags {
                    this.tags = tags.clone();
                }
                if let Err(e) = result {
                    error!(error = %e, ?op, "tag operation failed");
                    this.error = Some(format!("Tag 操作失败：{e}"));
                } else {
                    info!(?op, "tag operation completed");
                    if matches!(op, TagOp::Create { .. }) {
                        this.pending_clear_creation_inputs = true;
                    }
                    // 建/删 tag 会改变 commit 行的 ref 标签，历史列表同步刷新
                    if matches!(op, TagOp::Create { .. } | TagOp::Delete(_))
                        && (this.history_pane_visible || !this.history_commits.is_empty())
                    {
                        this.load_history_page(0, cx);
                    }
                    let msg = match &op {
                        TagOp::Create { name, .. } => format!("已创建 tag {name}"),
                        TagOp::Delete(name) => format!("已删除 tag {name}"),
                        TagOp::Push(name) => format!(
                            "已推送 tag {name} 到 {}",
                            push_remote.as_deref().unwrap_or("origin")
                        ),
                    };
                    this.notify_success(msg, cx);
                    if let Err(e) = &new_tags {
                        this.error = Some(format!("Tag 操作已完成，但刷新列表失败：{e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 「新建 tag」按钮：读 input → 调 run_tag_op
    ///
    /// message 非空时走 annotated tag；空则走 lightweight。
    pub(in crate::views) fn handle_create_tag(&mut self, cx: &mut Context<Self>) {
        if self
            .status
            .as_ref()
            .and_then(|status| status.head_commit.as_ref())
            .is_none()
        {
            self.error = Some("首次 commit 前没有可标记的版本，无法创建 Tag".into());
            cx.notify();
            return;
        }
        let name = self.create_tag_input.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.error = Some("tag 名不能为空".into());
            cx.notify();
            return;
        }
        let msg_raw = self
            .create_tag_message_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let message = if msg_raw.is_empty() {
            None
        } else {
            Some(msg_raw)
        };
        self.run_tag_op(TagOp::Create { name, message }, cx);
    }
}
