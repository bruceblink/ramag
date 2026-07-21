//! Commit graph：可分页续算的 lane 分配 + 着色 + 左侧 gutter 渲染。

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, hash_map::Entry};

use gpui::{AnyElement, IntoElement, ParentElement, Styled, div, px};
use gpui_component::{Icon, Sizable as _, h_flex, v_flex};
use ramag_domain::entities::{Commit, CommitId};

/// 单条 commit 在 history 视图中的图谱位置
#[derive(Debug, Clone)]
pub(super) struct CommitGraphRow {
    /// 该 commit 占的 lane 索引（0 = 最左）
    pub(super) lane: u8,
    /// 当前行总共有多少条活跃 lane（决定 gutter 宽度）
    pub(super) total_lanes: u8,
    /// 是否 merge commit（多 parent，dot 替换为 git-merge 图标）
    pub(super) is_merge: bool,
}

/// 单条 lane 宽度（px）：要小于 commit dot 直径，让线刚好被 dot 覆盖
const LANE_WIDTH: f32 = 14.0;
/// 病态分支图可能同时保留成千上万条逻辑 lane；展示层合并尾部 lane，避免每一行创建无界 UI 节点。
const MAX_RENDERED_LANES: usize = 64;

/// 可跨历史分页续算的 lane 状态；append 只处理新提交，不重扫既有页面。
#[derive(Default)]
pub(super) struct CommitLaneState {
    occupied: Vec<bool>,
    lane_by_commit: ActiveLanes,
    free_lanes: BinaryHeap<Reverse<usize>>,
}

const SMALL_ACTIVE_LANES: usize = 16;

#[derive(Default)]
struct ActiveLanes {
    small: Vec<(CommitKey, usize)>,
    large: Option<HashMap<CommitKey, usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum CommitKey {
    Sha1([u8; 40]),
    Sha256([u8; 64]),
    Other(String),
}

impl From<&CommitId> for CommitKey {
    fn from(id: &CommitId) -> Self {
        match id.0.as_bytes() {
            bytes if bytes.len() == 40 => {
                let mut key = [0; 40];
                key.copy_from_slice(bytes);
                Self::Sha1(key)
            }
            bytes if bytes.len() == 64 => {
                let mut key = [0; 64];
                key.copy_from_slice(bytes);
                Self::Sha256(key)
            }
            _ => Self::Other(id.0.clone()),
        }
    }
}

impl ActiveLanes {
    fn remove(&mut self, key: &CommitKey) -> Option<usize> {
        if let Some(large) = self.large.as_mut() {
            return large.remove(key);
        }
        let index = self
            .small
            .iter()
            .position(|(candidate, _)| candidate == key)?;
        Some(self.small.swap_remove(index).1)
    }

    /// 返回 true 表示成功插入；false 表示 key 已存在。
    fn insert_if_absent(&mut self, key: CommitKey, lane: usize) -> bool {
        if let Some(large) = self.large.as_mut() {
            return match large.entry(key) {
                Entry::Occupied(_) => false,
                Entry::Vacant(entry) => {
                    entry.insert(lane);
                    true
                }
            };
        }
        if self.small.iter().any(|(candidate, _)| candidate == &key) {
            return false;
        }
        if self.small.len() < SMALL_ACTIVE_LANES {
            self.small.push((key, lane));
            return true;
        }
        let mut large = HashMap::with_capacity(self.small.len().saturating_mul(2));
        large.extend(self.small.drain(..));
        large.insert(key, lane);
        self.large = Some(large);
        true
    }
}

impl CommitLaneState {
    pub(super) fn append<T>(&mut self, commits: &[T]) -> Vec<CommitGraphRow>
    where
        T: std::borrow::Borrow<Commit>,
    {
        let mut rows = Vec::with_capacity(commits.len());
        for commit in commits {
            let c = commit.borrow();
            let commit_key = CommitKey::from(&c.id);
            let lane = self
                .lane_by_commit
                .remove(&commit_key)
                .unwrap_or_else(|| allocate_lane(&mut self.occupied, &mut self.free_lanes));
            let is_merge = c.parents.len() > 1;

            if let Some(first_parent) = c.parents.first() {
                if self
                    .lane_by_commit
                    .insert_if_absent(CommitKey::from(first_parent), lane)
                {
                    self.occupied[lane] = true;
                } else {
                    release_lane(lane, &mut self.occupied, &mut self.free_lanes);
                }
            } else {
                release_lane(lane, &mut self.occupied, &mut self.free_lanes);
            }

            for parent in c.parents.iter().skip(1) {
                let parent_lane = allocate_lane(&mut self.occupied, &mut self.free_lanes);
                if !self
                    .lane_by_commit
                    .insert_if_absent(CommitKey::from(parent), parent_lane)
                {
                    release_lane(parent_lane, &mut self.occupied, &mut self.free_lanes);
                }
            }

            while self.occupied.last() == Some(&false) {
                self.occupied.pop();
            }
            let logical_total = self.occupied.len().max(lane + 1);
            let rendered_lane = lane.min(MAX_RENDERED_LANES - 1);
            let total_lanes = logical_total.min(MAX_RENDERED_LANES).max(rendered_lane + 1);
            rows.push(CommitGraphRow {
                lane: u8::try_from(rendered_lane).unwrap_or(u8::MAX),
                total_lanes: u8::try_from(total_lanes).unwrap_or(u8::MAX),
                is_merge,
            });
        }
        rows
    }
}

/// commit 时间倒序 → lane 分配。线性近似：active 维护各 lane 待入 commit，
/// 当前 commit 占其位、空位复用、新 lane 增长；first parent 替换占位，已存在则合并；其余 parent 起新 lane
#[cfg(test)]
pub(super) fn build_commit_lanes<T>(commits: &[T]) -> Vec<CommitGraphRow>
where
    T: std::borrow::Borrow<Commit>,
{
    CommitLaneState::default().append(commits)
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
    let total = usize::from(graph.total_lanes).clamp(1, MAX_RENDERED_LANES);
    let current_lane = usize::from(graph.lane).min(total - 1);
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
    fn active_lanes_promote_without_losing_entries() {
        let mut lanes = ActiveLanes::default();
        for index in 0..=SMALL_ACTIVE_LANES {
            assert!(lanes.insert_if_absent(CommitKey::Other(format!("c{index}")), index));
        }
        assert!(lanes.large.is_some());
        assert!(!lanes.insert_if_absent(CommitKey::Other("c0".into()), 99));
        for index in 0..=SMALL_ACTIVE_LANES {
            assert_eq!(
                lanes.remove(&CommitKey::Other(format!("c{index}"))),
                Some(index)
            );
        }
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
    fn paged_lane_build_matches_one_shot_build() {
        let commits = vec![
            mk("m", &["p1", "p2"]),
            mk("p1", &["r"]),
            mk("p2", &["r"]),
            mk("r", &[]),
        ];
        let expected = build_commit_lanes(&commits);
        let mut state = CommitLaneState::default();
        let mut actual = state.append(&commits[..2]);
        actual.extend(state.append(&commits[2..]));

        assert_eq!(actual.len(), expected.len());
        assert!(actual.iter().zip(expected).all(|(left, right)| {
            left.lane == right.lane
                && left.total_lanes == right.total_lanes
                && left.is_merge == right.is_merge
        }));
    }

    #[test]
    #[ignore = "手动观察十万条提交图布局耗时"]
    fn reports_large_linear_graph_latency() {
        use std::hint::black_box;
        use std::time::Instant;

        const COMMITS: usize = 100_000;
        const ITERATIONS: usize = 5;

        let commits = (0..COMMITS)
            .rev()
            .map(|index| {
                let id = format!("{index:040x}");
                if index == 0 {
                    mk(&id, &[])
                } else {
                    let parent = format!("{:040x}", index - 1);
                    mk(&id, &[&parent])
                }
            })
            .collect::<Vec<_>>();
        let mut samples = Vec::with_capacity(ITERATIONS);
        for _ in 0..ITERATIONS {
            let started = Instant::now();
            black_box(CommitLaneState::default().append(&commits));
            samples.push(started.elapsed());
        }
        samples.sort_unstable();
        eprintln!(
            "vcs commit graph: commits={COMMITS}, median={:.3} ms, row_bytes={}",
            samples[ITERATIONS / 2].as_secs_f64() * 1_000.0,
            std::mem::size_of::<CommitGraphRow>()
        );
    }

    #[test]
    fn pathological_graph_lane_count_is_bounded_for_rendering() {
        let parents: Vec<String> = (0..(MAX_RENDERED_LANES + 20))
            .map(|index| format!("p{index}"))
            .collect();
        let parent_refs: Vec<&str> = parents.iter().map(String::as_str).collect();
        let commits = vec![mk("merge", &parent_refs)];

        let rows = build_commit_lanes(&commits);

        assert_eq!(usize::from(rows[0].total_lanes), MAX_RENDERED_LANES);
        assert!(usize::from(rows[0].lane) < MAX_RENDERED_LANES);
    }

    #[test]
    fn lane_color_is_deterministic_per_lane() {
        let c0 = lane_color(0);
        let c1 = lane_color(1);
        assert!((c0.h - c1.h).abs() > 0.001);
        assert!((c0.h - lane_color(0).h).abs() < 1e-6);
    }
}
