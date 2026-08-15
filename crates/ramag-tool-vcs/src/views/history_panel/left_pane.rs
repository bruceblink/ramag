//! 历史视图左侧分支面板。

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement, Styled, px,
    uniform_list,
};
use gpui_component::v_flex;

use super::super::sidebar::{LeftRow, SidebarSection};
use super::super::vcs_view::VcsView;
use super::{HistoryLeftRowsCacheEntry, HistoryLeftRowsCacheKey};

impl VcsView {
    pub(super) fn render_history_left_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.history_left_rows();
        let body = uniform_list(
            "vcs-history-left-rows",
            rows.len(),
            cx.processor({
                let rows = rows.clone();
                move |this, range: Range<usize>, _window, cx| {
                    range
                        .map(|index| this.render_left_row(&rows[index], cx))
                        .collect::<Vec<_>>()
                }
            }),
        )
        .track_scroll(&self.history_left_scroll)
        .flex_1();

        v_flex()
            .id("vcs-history-left-pane")
            .size_full()
            .min_h_0()
            .px(px(8.0))
            .py(px(6.0))
            .child(body)
            .into_any_element()
    }

    fn history_left_rows(&self) -> Rc<Vec<LeftRow>> {
        let key = HistoryLeftRowsCacheKey {
            local_identity: self.local_branches.as_ptr() as usize,
            local_len: self.local_branches.len(),
            remote_identity: self.remote_branches.as_ptr() as usize,
            remote_len: self.remote_branches.len(),
            tags_identity: self.tags.as_ptr() as usize,
            tags_len: self.tags.len(),
            remotes_identity: self.remotes.as_ptr() as usize,
            remotes_len: self.remotes.len(),
            collapsed_local: self.collapsed_local,
            collapsed_remote: self.collapsed_remote,
            collapsed_tag: self.collapsed_tag,
            collapsed_remote_repos: self.collapsed_remote_repos,
        };
        {
            let cache = self.history_left_rows_cache.borrow();
            if let Some(rows) = cache.as_ref().and_then(|entry| entry.get(&key)) {
                return rows;
            }
        }

        let mut rows = Vec::new();
        rows.push(LeftRow::Header {
            title: "本地分支",
            count: self.local_branches.len(),
            collapsed: self.collapsed_local,
            section: SidebarSection::Local,
        });
        if !self.collapsed_local {
            for index in 0..self.local_branches.len() {
                rows.push(LeftRow::Branch {
                    idx: index,
                    is_remote: false,
                });
            }
        }

        rows.push(LeftRow::Header {
            title: "远程分支",
            count: self.remote_branches.len(),
            collapsed: self.collapsed_remote,
            section: SidebarSection::Remote,
        });
        if !self.collapsed_remote {
            if self.remote_branches.is_empty() {
                rows.push(LeftRow::Empty("暂无远程分支；获取后显示"));
            } else {
                for index in 0..self.remote_branches.len() {
                    rows.push(LeftRow::Branch {
                        idx: index,
                        is_remote: true,
                    });
                }
            }
        }

        rows.push(LeftRow::Header {
            title: "远程仓库",
            count: self.remotes.len(),
            collapsed: self.collapsed_remote_repos,
            section: SidebarSection::RemoteRepo,
        });
        if !self.collapsed_remote_repos {
            if self.remotes.is_empty() {
                rows.push(LeftRow::Empty("暂无远程仓库"));
            } else {
                for index in 0..self.remotes.len() {
                    rows.push(LeftRow::Remote { idx: index });
                }
            }
        }

        rows.push(LeftRow::Header {
            title: "标签",
            count: self.tags.len(),
            collapsed: self.collapsed_tag,
            section: SidebarSection::Tag,
        });
        if !self.collapsed_tag {
            if self.tags.is_empty() {
                rows.push(LeftRow::Empty("暂无标签"));
            } else {
                for index in 0..self.tags.len() {
                    rows.push(LeftRow::Tag { idx: index });
                }
            }
        }

        let rows = Rc::new(rows);
        self.history_left_rows_cache
            .replace(Some(HistoryLeftRowsCacheEntry {
                key,
                rows: rows.clone(),
            }));
        rows
    }
}
