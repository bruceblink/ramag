//! Hash / ZSet / Stream 共用双列行编辑器。collect 时一次返回首个错误（含行号）

use gpui::{
    App, ClickEvent, Context, Entity, InteractiveElement as _, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _,
    button::ButtonVariants as _,
    input::{Input, InputState},
    v_flex,
};
use ramag_domain::entities::MAX_REDIS_COMMAND_ARG_BYTES;

use crate::views::{bounded_input, reserve_command_input_bytes};

const MAX_EDITOR_ROWS: usize = 200;
const MAX_SCORE_INPUT_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairsKind {
    Hash,
    ZSet,
    Stream,
}

struct PairRow {
    id: u64,
    left: Entity<InputState>,
    right: Entity<InputState>,
}

pub struct PairsEditor {
    kind: PairsKind,
    rows: Vec<PairRow>,
    next_id: u64,
    disabled: bool,
}

impl PairsEditor {
    pub fn new(kind: PairsKind, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut me = Self {
            kind,
            rows: Vec::new(),
            next_id: 0,
            disabled: false,
        };
        me.add_row(window, cx);
        me
    }

    fn add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled || self.rows.len() >= MAX_EDITOR_ROWS {
            return;
        }
        let (lph, rph) = placeholders(self.kind);
        let left_limit = if matches!(self.kind, PairsKind::ZSet) {
            MAX_SCORE_INPUT_BYTES
        } else {
            MAX_REDIS_COMMAND_ARG_BYTES
        };
        let left = cx.new(|cx| bounded_input(left_limit, window, cx).placeholder(lph));
        let right =
            cx.new(|cx| bounded_input(MAX_REDIS_COMMAND_ARG_BYTES, window, cx).placeholder(rph));
        let id = self.next_id;
        self.next_id += 1;
        self.rows.push(PairRow { id, left, right });
        cx.notify();
    }

    fn remove_row(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.disabled || self.rows.len() <= 1 {
            return;
        }
        self.rows.retain(|r| r.id != id);
        cx.notify();
    }

    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        if self.disabled != disabled {
            self.disabled = disabled;
            cx.notify();
        }
    }

    /// 收集 + 行级校验
    /// - 整行（左右皆）空 → 跳过
    /// - 否则按 kind 校验 left；失败返回带行号的错误
    pub fn collect(&self, cx: &App) -> Result<Vec<(String, String)>, String> {
        let mut out = Vec::new();
        let mut total_bytes = 0usize;
        for (idx, row) in self.rows.iter().enumerate() {
            let left_input = row.left.read(cx);
            let right_input = row.right.read(cx);
            let left_value = left_input.value();
            let right_value = right_input.value();
            let left_raw = left_value.as_ref();
            let right = right_value.as_ref();
            if left_raw.is_empty() && right.is_empty() {
                continue;
            }
            let left = match self.kind {
                PairsKind::Hash | PairsKind::Stream => {
                    if left_raw.is_empty() {
                        return Err(format!("第 {} 行：字段名不能为空", idx + 1));
                    }
                    // Redis 字段名是二进制安全参数，保留合法的前后空格。
                    left_raw
                }
                PairsKind::ZSet => {
                    let left = left_raw.trim();
                    if left.is_empty() {
                        return Err(format!("第 {} 行：score 不能为空", idx + 1));
                    }
                    if !left.parse::<f64>().is_ok_and(|score| !score.is_nan()) {
                        return Err(format!(
                            "第 {} 行：score 必须是数字（如 1.5），实得 `{left}`",
                            idx + 1
                        ));
                    }
                    if right.is_empty() {
                        return Err(format!("第 {} 行：成员名不能为空", idx + 1));
                    }
                    left
                }
            };
            let pair_bytes = left
                .len()
                .checked_add(right.len())
                .ok_or_else(|| format!("第 {} 行：字段和值的总长度溢出", idx + 1))?;
            let Some(next_bytes) = reserve_command_input_bytes(total_bytes, pair_bytes) else {
                return Err("本次批量输入超过 16 MiB 总上限，请拆分提交".into());
            };
            total_bytes = next_bytes;
            out.push((left.to_string(), right.to_string()));
        }
        Ok(out)
    }
}

impl Render for PairsEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;

        let (count_unit, left_width) = match self.kind {
            PairsKind::Hash => ("字段", 180.0_f32),
            PairsKind::ZSet => ("成员", 120.0_f32),
            PairsKind::Stream => ("字段", 180.0_f32),
        };

        let toolbar = ramag_ui::responsive_toolbar()
            .debug_selector(|| "redis-pairs-toolbar".into())
            .w_full()
            .gap(px(10.0))
            .child(
                ramag_ui::clickable_button("pe-add")
                    .outline()
                    .small()
                    .flex_none()
                    .debug_selector(|| "redis-pairs-add".into())
                    .icon(IconName::Plus)
                    .label("添加")
                    .disabled(self.disabled || self.rows.len() >= MAX_EDITOR_ROWS)
                    .when(self.rows.len() >= MAX_EDITOR_ROWS, |button| {
                        button.tooltip("最多 200 行")
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.add_row(window, cx);
                    })),
            )
            .child(
                div()
                    .debug_selector(|| "redis-pairs-count".into())
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(muted_fg)
                    .whitespace_normal()
                    .child(format!("{} 个{count_unit}", self.rows.len())),
            );

        let mut list = v_flex().w_full().gap(px(6.0));
        for row in &self.rows {
            let id = row.id;
            let mut line = ramag_ui::responsive_toolbar()
                .debug_selector({
                    let id = row.id;
                    move || format!("redis-pairs-row-{id}")
                })
                .w_full()
                .gap(px(8.0))
                .child(
                    div()
                        .debug_selector({
                            let id = row.id;
                            move || format!("redis-pairs-left-{id}")
                        })
                        .flex_1()
                        .min_w(px(96.0))
                        .max_w(px(left_width))
                        .child(Input::new(&row.left).disabled(self.disabled)),
                )
                .child(
                    div()
                        .debug_selector({
                            let id = row.id;
                            move || format!("redis-pairs-right-{id}")
                        })
                        .flex_1()
                        .min_w(px(96.0))
                        .child(Input::new(&row.right).disabled(self.disabled)),
                );
            if self.rows.len() > 1 {
                line = line.child(
                    ramag_ui::clickable_button(SharedString::from(format!("pe-rm-{id}")))
                        .ghost()
                        .small()
                        .flex_none()
                        .debug_selector(move || format!("redis-pairs-remove-{id}"))
                        .icon(IconName::Close)
                        .tooltip("删除")
                        .disabled(self.disabled)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.remove_row(id, cx);
                        })),
                );
            }
            list = list.child(line);
        }

        // toolbar 放底部：行列表在上，添加/计数 chip 在下，避免左上角先看到操作按钮
        v_flex()
            .debug_selector(|| "redis-pairs-editor".into())
            .w_full()
            .gap(px(10.0))
            .child(list)
            .child(toolbar)
    }
}

fn placeholders(kind: PairsKind) -> (&'static str, &'static str) {
    match kind {
        PairsKind::Hash => ("字段名（如 name）", "字段值"),
        PairsKind::ZSet => ("score（数字）", "成员名"),
        PairsKind::Stream => ("字段名", "字段值"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_distinct_per_kind() {
        let h = placeholders(PairsKind::Hash);
        let z = placeholders(PairsKind::ZSet);
        let s = placeholders(PairsKind::Stream);
        assert_ne!(h.0, z.0);
        assert_ne!(z.0, s.0);
        assert!(z.0.contains("score"));
    }
}

#[cfg(test)]
#[path = "pairs_editor_render_test.rs"]
mod render_test;
