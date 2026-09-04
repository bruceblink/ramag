use super::*;

use ramag_ui::RestrictScrollToAxisExt as _;

impl TableDesigner {
    pub(super) fn render_field_editor(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted;
        let muted_fg = theme.muted_foreground;
        let syntax = &theme.highlight_theme.style.syntax;
        let type_color = syntax_color(syntax, "type", theme.link);
        let keyword_color = syntax_color(syntax, "keyword", theme.info);
        let number_color = syntax_color(syntax, "number", theme.warning);
        let string_color = syntax_color(syntax, "string", theme.success);
        let constant_color = syntax_color(syntax, "constant", theme.foreground);
        let entity = cx.entity().clone();
        let reviewing = self.preview_sql.is_some();
        let active_fields = self.fields.iter().filter(|field| !field.deleted).count();
        let visible_rows = visible_field_rows(active_fields, reviewing);
        let rows_height = px(visible_rows as f32 * FIELD_ROW_HEIGHT);
        let mut rows = v_flex().w_full();

        for (index, field) in self.fields.iter().enumerate() {
            if field.deleted {
                continue;
            }
            let toggle = entity.clone();
            let remove = entity.clone();
            rows =
                rows.child(
                    h_flex()
                        .w_full()
                        .min_h(px(FIELD_ROW_HEIGHT))
                        .items_center()
                        .gap_2()
                        .px_3()
                        .border_t_1()
                        .border_color(border)
                        .when(index % 2 == 1, |row| row.bg(muted.opacity(0.45)))
                        .child(
                            Input::new(&field.name)
                                .w(px(170.0))
                                .disabled(reviewing || self.executing),
                        )
                        .child(
                            Input::new(&field.data_type)
                                .w(px(180.0))
                                .font_family(theme.mono_font_family.clone())
                                .text_color(type_color)
                                .disabled(reviewing || self.executing),
                        )
                        .child(
                            h_flex().w(px(76.0)).items_center().justify_center().child(
                                ramag_ui::clickable_checkbox(format!("field-nullable-{index}"))
                                    .checked(field.nullable)
                                    .small()
                                    .disabled(reviewing || self.executing)
                                    .tooltip("允许 NULL")
                                    .on_click(move |nullable: &bool, _, app| {
                                        toggle.update(app, |this, cx| {
                                            if let Some(field) = this.fields.get_mut(index) {
                                                field.nullable = *nullable;
                                            }
                                            this.preview_sql = None;
                                            this.discard_confirming = false;
                                            cx.notify();
                                        })
                                    }),
                            ),
                        )
                        .child(
                            Input::new(&field.default_value)
                                .w(px(180.0))
                                .font_family(theme.mono_font_family.clone())
                                .text_color(default_value_color(
                                    field.default_value.read(cx).value().as_ref(),
                                    keyword_color,
                                    number_color,
                                    string_color,
                                    constant_color,
                                ))
                                .disabled(reviewing || self.executing),
                        )
                        .child(div().flex_1().min_w(px(150.0)).child(
                            Input::new(&field.comment).disabled(reviewing || self.executing),
                        ))
                        .child(
                            ramag_ui::clickable_button(format!("field-delete-{index}"))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Delete)
                                .tooltip("删除")
                                .text_color(theme.danger)
                                .disabled(reviewing || self.executing)
                                .on_click(move |_: &ClickEvent, _, app| {
                                    remove.update(app, |this, cx| {
                                        if let Some(field) = this.fields.get_mut(index) {
                                            field.deleted = true;
                                        }
                                        this.preview_sql = None;
                                        this.discard_confirming = false;
                                        cx.notify();
                                    })
                                }),
                        ),
                );
        }

        v_flex()
            .w_full()
            .min_w_0()
            .relative()
            .border_1()
            .border_color(border)
            .rounded_lg()
            .overflow_hidden()
            .child(
                div()
                    .id("table-designer-fields-h-scroll")
                    .debug_selector(|| "table-designer-fields-h-scroll".into())
                    .relative()
                    .w_full()
                    .min_w_0()
                    .overflow_x_scroll()
                    .restrict_scroll_to_axis()
                    .track_scroll(&self.field_horizontal_scroll)
                    .child(
                        v_flex()
                            .id("table-designer-fields-content")
                            .debug_selector(|| "table-designer-fields-content".into())
                            .w_full()
                            .min_w(px(FIELD_TABLE_MIN_WIDTH))
                            .child(
                                h_flex()
                                    .min_h(px(38.0))
                                    .items_center()
                                    .gap_2()
                                    .px_3()
                                    .bg(muted.opacity(0.7))
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(muted_fg)
                                    .child(div().w(px(170.0)).child("字段名"))
                                    .child(div().w(px(180.0)).child("类型"))
                                    .child(div().w(px(76.0)).text_center().child("允许 NULL"))
                                    .child(div().w(px(180.0)).child("默认值"))
                                    .child(div().flex_1().min_w(px(150.0)).child("注释"))
                                    .child(div().w(px(36.0)).text_center().child("操作")),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .h(rows_height)
                                    .min_h_0()
                                    .flex_none()
                                    .id("table-designer-fields-scroll")
                                    .overflow_y_scroll()
                                    .track_scroll(&self.field_scroll)
                                    .child(rows),
                            ),
                    ),
            )
            .child(
                div()
                    .id("table-designer-fields-scroll-input")
                    .absolute()
                    .inset_0()
                    .on_scroll_wheel(cx.listener(Self::on_field_scroll)),
            )
    }
}
