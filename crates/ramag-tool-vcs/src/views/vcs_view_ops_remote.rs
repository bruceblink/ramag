//! 远程列表加载，由 vcs_view_ops_repo 在 open_repo 后调

use gpui::Context;
use tracing::error;

use super::vcs_view::VcsView;

impl VcsView {
    /// 异步加载 remote 列表
    pub(super) fn reload_remotes(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        self.loading_remotes = true;
        self.remotes_request_seq = self.remotes_request_seq.wrapping_add(1);
        let request_seq = self.remotes_request_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = driver.list_remotes(&repo).await;
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo) || this.remotes_request_seq != request_seq {
                    return;
                }
                this.loading_remotes = false;
                match result {
                    Ok(list) => this.remotes = list,
                    Err(e) => {
                        error!(
                            operation = "vcs_remote_list",
                            repo_id = %repo,
                            error = %e,
                            "load remotes failed"
                        );
                        this.error = Some(format!("加载远程列表失败：{e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
