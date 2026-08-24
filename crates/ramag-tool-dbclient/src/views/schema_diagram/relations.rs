use std::collections::HashSet;

use gpui::{IntoElement, ParentElement, Styled, div, px};
use gpui_component::{Icon, IconName, Sizable as _, Theme, h_flex, v_flex};

use super::{DiagramRelation, SchemaDiagramPanel};

impl SchemaDiagramPanel {
    /// Renders the relation list below the grid so edges remain readable without a canvas.
    pub(super) fn render_relation_summary(
        &self,
        relations: &[DiagramRelation],
        visible_table_names: &HashSet<&str>,
        theme: &Theme,
    ) -> impl IntoElement {
        let muted_fg = theme.muted_foreground;
        let border = theme.border;
        let fg = theme.foreground;
        let visible_relations: Vec<&DiagramRelation> = relations
            .iter()
            .filter(|relation| visible_table_names.contains(relation.source_table.as_str()))
            .collect();
        let mut section = v_flex()
            .w_full()
            .mt(px(16.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(border)
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .px_2()
                    .py(px(6.0))
                    .bg(theme.secondary)
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(fg)
                            .child(format!("关系 ({})", visible_relations.len())),
                    ),
            );
        for relation in visible_relations.iter().take(64) {
            section = section.child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py(px(4.0))
                    .child(
                        div()
                            .w(px(190.0))
                            .flex_none()
                            .text_xs()
                            .text_color(fg)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(format!("{}.{}", relation.source_table, relation.name)),
                    )
                    .child(
                        Icon::new(IconName::ArrowRight)
                            .xsmall()
                            .text_color(muted_fg),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(muted_fg)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(format!(
                                "{}.{} ({}) → ({})",
                                relation.ref_schema,
                                relation.ref_table,
                                relation.columns.join(", "),
                                relation.ref_columns.join(", ")
                            )),
                    ),
            );
        }
        if visible_relations.len() > 64 {
            section = section.child(
                div()
                    .px_2()
                    .py(px(4.0))
                    .text_xs()
                    .text_color(muted_fg)
                    .child(format!("… 还有 {} 个关系", visible_relations.len() - 64)),
            );
        }
        section
    }
}
