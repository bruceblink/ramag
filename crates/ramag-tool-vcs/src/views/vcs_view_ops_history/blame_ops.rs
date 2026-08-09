//! VcsView blame 相关 ops：行级 inline blame banner / 完整 blame 加载 / 切换 diff↔blame 视图

use gpui::{Context, SharedString};
use tracing::error;

use super::super::vcs_view::VcsView;

impl VcsView {
    /// 行号点击 → 拉当前工作区文件 blame，命中行写到顶部 banner。
    /// 旧侧行号对应的是变更前内容，不能拿当前 HEAD 的 blame 冒充，直接忽略。
    pub(crate) fn show_inline_blame(&mut self, line_no: u32, is_old: bool, cx: &mut Context<Self>) {
        if is_old {
            return;
        }
        let Some(path) = self.selected_file.as_ref().map(|(p, _)| p.clone()) else {
            return;
        };
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        self.inline_blame_request_seq = self.inline_blame_request_seq.wrapping_add(1);
        let request_seq = self.inline_blame_request_seq;
        self.inline_blame_text = Some("加载行作者信息…".into());
        cx.notify();
        let driver = self.driver.clone();
        cx.spawn(async move |this, cx| {
            let result = driver.blame(&repo, &path).await;
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo)
                    || this.inline_blame_request_seq != request_seq
                    || this.selected_file.as_ref().map(|(p, _)| p.as_str()) != Some(path.as_str())
                {
                    return;
                }
                match result {
                    Ok(lines) => {
                        if let Some(b) = lines.iter().find(|l| l.line_no == line_no) {
                            let short = b.commit.0.chars().take(7).collect::<String>();
                            let date = b.timestamp.format("%Y-%m-%d");
                            this.inline_blame_text = Some(SharedString::from(format!(
                                "L{line_no}　{short}　·　{}　·　{date}　·　{}",
                                super::super::inline_text_preview(&b.author, 40),
                                super::super::inline_text_preview(&b.subject, 160)
                            )));
                        } else {
                            this.inline_blame_text =
                                Some(SharedString::from(format!("L{line_no}：未找到 blame 信息")));
                        }
                    }
                    Err(e) => {
                        error!(
                            operation = "vcs_inline_blame",
                            repo_id = %repo,
                            path = %path,
                            line_no,
                            error = %e,
                            "inline blame failed"
                        );
                        this.inline_blame_text =
                            Some(SharedString::from(format!("blame 失败：{e}")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 清空 inline blame banner（用户切文件 / 关闭按钮 / 切视图时调）
    pub(crate) fn clear_inline_blame(&mut self, cx: &mut Context<Self>) {
        self.inline_blame_request_seq = self.inline_blame_request_seq.wrapping_add(1);
        if self.inline_blame_text.is_some() {
            self.inline_blame_text = None;
            cx.notify();
        }
    }

    /// 切换 diff/blame 视图；showing_blame=true 拉 blame，否则清空
    /// 只支持 Changes：历史 commit diff 需要按指定 revision blame，不能用当前 HEAD 数据冒充。
    pub(crate) fn toggle_blame(&mut self, cx: &mut Context<Self>) {
        self.showing_blame = !self.showing_blame;
        if self.showing_blame {
            let path = self.selected_file.as_ref().map(|(p, _)| p.clone());
            if let Some(p) = path {
                self.load_blame(p, cx);
            } else {
                self.showing_blame = false;
            }
        } else {
            self.blame_lines = std::rc::Rc::new(Vec::new());
        }
        cx.notify();
    }

    /// 异步拉取指定文件的 blame
    pub(crate) fn load_blame(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        self.loading_blame = true;
        self.blame_lines = std::rc::Rc::new(Vec::new());
        self.blame_request_seq = self.blame_request_seq.wrapping_add(1);
        let request_seq = self.blame_request_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = driver.blame(&repo, &path).await;
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo) || this.blame_request_seq != request_seq {
                    return;
                }
                // 请求身份校验：blame 目标文件已切换时，旧回包不写入（防串文件）
                let target = this.selected_file.as_ref().map(|(p, _)| p.clone());
                if target.as_deref() != Some(path.as_str()) {
                    cx.notify();
                    return;
                }
                this.loading_blame = false;
                match result {
                    Ok(lines) => this.blame_lines = std::rc::Rc::new(lines),
                    Err(e) => {
                        error!(
                            operation = "vcs_blame",
                            repo_id = %repo,
                            path = %path,
                            error = %e,
                            "blame failed"
                        );
                        this.error = Some(format!("Blame 失败：{e}"));
                        this.showing_blame = false;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
