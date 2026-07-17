//! Commit graph：lane 分配 + 着色 + 左侧 gutter 渲染。算法见 `build_commit_lanes`

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use gpui::{AnyElement, IntoElement, ParentElement, Styled, div, px};
use gpui_component::{Icon, Sizable as _, h_flex, v_flex};
use ramag_domain::entities::{Commit, CommitId};

/// 单条 commit 在 history 视图中的图谱位置
#[derive(Debug, Clone)]
pub(super) struct CommitGraphRow {
    /// 该 commit 占的 lane 索引（0 = 最左）
    pub(super) lane: usize,
    /// 当前行总共有多少条活跃 lane（决定 gutter 宽度）
    pub(super) total_lanes: usize,
    /// 是否 merge commit（多 parent，dot 替换为 git-merge 图标）
    pub(super) is_merge: bool,
}

/// 单条 lane 宽度（px）：要小于 commit dot 直径，让线刚好被 dot 覆盖
const LANE_WIDTH: f32 = 14.0;
/// 病态分支图可能同时保留成千上万条逻辑 lane；展示层合并尾部 lane，避免每一行创建无界 UI 节点。
const MAX_RENDERED_LANES: usize = 64;

/// commit 时间倒序 → lane 分配。线性近似：active 维护各 lane 待入 commit，
/// 当前 commit 占其位、空位复用、新 lane 增长；first parent 替换占位，已存在则合并；其余 parent 起新 lane
pub(super) fn build_commit_lanes<T>(commits: &[T]) -> Vec<CommitGraphRow>
where
    T: std::borrow::Borrow<Commit>,
{
    let mut occupied: Vec<bool> = Vec::new();
    let mut lane_by_commit: HashMap<CommitId, usize> = HashMap::new();
    let mut free_lanes: BinaryHeap<Reverse<usize>> = BinaryHeap::new();
    let mut rows: Vec<CommitGraphRow> = Vec::with_capacity(commits.len());

    for commit in commits {
        let c = commit.borrow();
        let lane = lane_by_commit
            .remove(&c.id)
            .unwrap_or_else(|| allocate_lane(&mut occupied, &mut free_lanes));
        let is_merge = c.parents.len() > 1;

        if let Some(p0) = c.parents.first() {
            if lane_by_commit.contains_key(p0) {
                release_lane(lane, &mut occupied, &mut free_lanes);
            } else {
                occupied[lane] = true;
                lane_by_commit.insert(p0.clone(), lane);
            }
        } else {
            release_lane(lane, &mut occupied, &mut free_lanes);
        }

        for p in c.parents.iter().skip(1) {
            if lane_by_commit.contains_key(p) {
                continue;
            }
            let parent_lane = allocate_lane(&mut occupied, &mut free_lanes);
            lane_by_commit.insert(p.clone(), parent_lane);
        }

        while occupied.last() == Some(&false) {
            occupied.pop();
        }
        let logical_total = occupied.len().max(lane + 1);
        let rendered_lane = lane.min(MAX_RENDERED_LANES - 1);
        let total_lanes = logical_total.min(MAX_RENDERED_LANES).max(rendered_lane + 1);

        rows.push(CommitGraphRow {
            lane: rendered_lane,
            total_lanes,
            is_merge,
        });
    }
    rows
}

fn allocate_lane(occupied: &mut Vec<bool>, free_lanes: &mut BinaryHeap<Reverse<usize>>) -> usize {
    while let Some(Reverse(lane)) = free_lanes.pop() {
        if occupied.get(lane) == Some(&false) {
            occupied[lane] = true;
            return lane;
        }
    }
    occupied.push(true);
    occupied.len() - 1
}

fn release_lane(lane: usize, occupied: &mut [bool], free_lanes: &mut BinaryHeap<Reverse<usize>>) {
    if occupied.get(lane) == Some(&true) {
        occupied[lane] = false;
        free_lanes.push(Reverse(lane));
    }
}

/// 给 lane 分配高对比度颜色（基于黄金角分布，相邻 lane 不会同色）
pub(super) fn lane_color(lane: usize) -> gpui::Hsla {
    // 黄金角 137.508°：连续 hash 后相邻值 hue 差最大
    let hue = (lane as f32 * 137.508) % 360.0;
    gpui::hsla(hue / 360.0, 0.55, 0.55, 1.0)
}

/// 渲染左侧 lane gutter：N 条彩色竖线 + 本 commit 所在 lane 的 dot
pub(super) fn render_lane_gutter(graph: &CommitGraphRow) -> AnyElement {
    let total = graph.total_lanes.clamp(1, MAX_RENDERED_LANES);
    let current_lane = graph.lane.min(total - 1);
    let mut row = h_flex().flex_none().items_stretch();
    for i in 0..total {
        let mut color = lane_color(i);
        let mut bg_line = color;
        bg_line.a = 0.45;
        let lane_div = if i == current_lane {
            let dot_icon: AnyElement = if graph.is_merge {
                Icon::new(ramag_ui::icons::git_merge())
                    .small()
                    .text_color(color)
                    .into_any_element()
            } else {
                color.a = 1.0;
                Icon::new(ramag_ui::icons::circle_dot())
                    .small()
                    .text_color(color)
                    .into_any_element()
            };
            v_flex()
                .flex_none()
                .w(px(LANE_WIDTH))
                .items_center()
                .child(div().w(px(2.0)).h(px(8.0)).bg(bg_line))
                .child(dot_icon)
                .child(div().w(px(2.0)).flex_1().bg(bg_line))
        } else {
            v_flex()
                .flex_none()
                .w(px(LANE_WIDTH))
                .items_center()
                .child(div().w(px(2.0)).h_full().bg(bg_line))
        };
        row = row.child(lane_div);
    }
    row.into_any_element()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use ramag_domain::entities::{CommitId, Signature};

    fn mk(id: &str, parents: &[&str]) -> Commit {
        let sig = Signature {
            name: "Author".into(),
            email: "a@e.com".into(),
            timestamp: Utc.timestamp_opt(0, 0).unwrap(),
        };
        Commit {
            id: CommitId(id.into()),
            parents: parents.iter().map(|p| CommitId((*p).into())).collect(),
            author: sig.clone(),
            committer: sig,
            subject: format!("commit {id}"),
            body: String::new(),
            refs: Vec::new(),
        }
    }

    #[test]
    fn linear_history_keeps_one_lane() {
        let commits = vec![mk("c", &["b"]), mk("b", &["a"]), mk("a", &[])];
        let rows = build_commit_lanes(&commits);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.lane == 0));
        assert!(rows.iter().all(|r| r.total_lanes == 1));
    }

    #[test]
    fn merge_commit_uses_two_lanes_then_collapses() {
        let commits = vec![
            mk("m", &["p1", "p2"]),
            mk("p1", &["r"]),
            mk("p2", &["r"]),
            mk("r", &[]),
        ];
        let rows = build_commit_lanes(&commits);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].lane, 0);
        assert!(rows[0].is_merge);
        assert_eq!(rows[0].total_lanes, 2);
        assert_eq!(rows[1].lane, 0);
        assert_eq!(rows[2].lane, 1);
        assert_eq!(rows[3].lane, 0);
        assert_eq!(rows[3].total_lanes, 1);
    }

    #[test]
    fn pathological_graph_lane_count_is_bounded_for_rendering() {
        let parents: Vec<String> = (0..(MAX_RENDERED_LANES + 20))
            .map(|index| format!("p{index}"))
            .collect();
        let parent_refs: Vec<&str> = parents.iter().map(String::as_str).collect();
        let commits = vec![mk("merge", &parent_refs)];

        let rows = build_commit_lanes(&commits);

        assert_eq!(rows[0].total_lanes, MAX_RENDERED_LANES);
        assert!(rows[0].lane < MAX_RENDERED_LANES);
    }

    #[test]
    fn lane_color_is_deterministic_per_lane() {
        let c0 = lane_color(0);
        let c1 = lane_color(1);
        assert!((c0.h - c1.h).abs() > 0.001);
        assert!((c0.h - lane_color(0).h).abs() < 1e-6);
    }
}
