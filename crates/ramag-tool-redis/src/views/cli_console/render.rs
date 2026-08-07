//! Redis CLI 控制台渲染。

use super::*;

impl Render for CliConsole {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let border = theme.border;
        let bg = theme.background;
        let secondary_bg = theme.secondary;
        let accent = theme.primary;
        let read_only_write = if self.config.production {
            let input = self.input.read(cx);
            let input_value = input.value();
            let value = input_value.trim();
            value.len() <= MAX_COMMAND_BYTES
                && format::tokenize(value)
                    .ok()
                    .and_then(|argv| argv.into_iter().next())
                    .is_some_and(|command| self.service.is_write_command(&command))
        } else {
            false
        };
        let pending_commands = pending_command_count(&self.history);
        let command_queue_full = pending_commands >= MAX_PENDING_COMMANDS;
        let history_label = if pending_commands == 0 {
            format!("命令行 · DB {} · {} 条", self.db, self.history.len())
        } else {
            format!(
                "命令行 · DB {} · {} 条 · {pending_commands} 执行中",
                self.db,
                self.history.len()
            )
        };

        let toolbar = h_flex()
            .w_full()
            .px(px(12.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(border)
            .bg(secondary_bg)
            .gap(px(8.0))
            .items_center()
            .child(div().text_xs().text_color(muted_fg).child(history_label))
            .when(self.config.production, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(gpui::red())
                        .child("只读：写命令已禁用"),
                )
            })
            .child(div().flex_1())
            .child(
                ramag_ui::clickable_button("cli-clear")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::trash())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.clear(cx))),
            );

        let transcript: gpui::AnyElement = if self.history.is_empty() {
            div()
                .p(px(12.0))
                .text_sm()
                .text_color(muted_fg)
                .child(
                    "尚无命令；输入并 Enter 执行（PING / GET foo / KEYS * / CONFIG GET maxmemory）",
                )
                .into_any_element()
        } else {
            div()
                .relative()
                .size_full()
                .child(
                    div()
                        .id("cli-transcript-hscroll")
                        .debug_selector(|| "cli-transcript-scroll-region".into())
                        .size_full()
                        .overflow_x_scroll()
                        .restrict_scroll_to_axis()
                        .track_scroll(&self.transcript_h_scroll)
                        .child(
                            uniform_list(
                                "cli-transcript",
                                self.transcript_rows.len(),
                                cx.processor(move |this, range: Range<usize>, _w, cx| {
                                    range
                                        .filter_map(|index| {
                                            let row = this.transcript_rows.get(index)?;
                                            Some(render_transcript_row(
                                                row, fg, muted_fg, accent, cx,
                                            ))
                                        })
                                        .collect()
                                }),
                            )
                            .track_scroll(&self.transcript_scroll)
                            .restrict_scroll_to_axis()
                            .h_full()
                            .w(px(DISPLAY_CONTENT_WIDTH_PX)),
                        ),
                )
                .child(
                    div()
                        .id("cli-transcript-scroll-input")
                        .absolute()
                        .inset_0()
                        .on_scroll_wheel(cx.listener(Self::on_transcript_scroll)),
                )
                .into_any_element()
        };

        let input_row = h_flex()
            .w_full()
            .px(px(12.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(border)
            .gap(px(8.0))
            .items_center()
            .child(div().text_xs().text_color(muted_fg).child("⏵"))
            .child(div().flex_1().min_w_0().child(Input::new(&self.input)))
            .child(
                ramag_ui::clickable_button("cli-run")
                    .primary()
                    .small()
                    .icon(IconName::Play)
                    .disabled(read_only_write || command_queue_full)
                    .when(read_only_write || command_queue_full, |button| {
                        button.tooltip(if read_only_write {
                            "只读"
                        } else {
                            "命令队列已满"
                        })
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.handle_submit(window, cx)
                    })),
            );

        v_flex()
            .size_full()
            .occlude()
            .bg(bg)
            // 输入组件不直接处理上下键，先交给补全菜单。
            .on_action(cx.listener(|this, _: &MoveUp, window, cx| {
                let handled = this.input.update(cx, |state, cx| {
                    state.handle_action_for_context_menu(Box::new(MoveUp), window, cx)
                });
                if !handled {
                    this.history_prev(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &MoveDown, window, cx| {
                let handled = this.input.update(cx, |state, cx| {
                    state.handle_action_for_context_menu(Box::new(MoveDown), window, cx)
                });
                if !handled {
                    this.history_next(window, cx);
                }
            }))
            .child(toolbar)
            .child(input_row)
            .child(div().flex_1().min_h_0().child(transcript))
    }
}

const ROW_H: f32 = 20.0;

fn render_transcript_row(
    row: &TranscriptRow,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    accent: gpui::Hsla,
    cx: &mut Context<CliConsole>,
) -> gpui::AnyElement {
    match row {
        TranscriptRow::Continue { entry_id, hint } => {
            let entry_id = *entry_id;
            h_flex()
                .h(px(ROW_H))
                .w_full()
                .px(px(12.0))
                .items_center()
                .child(
                    ramag_ui::clickable_button(SharedString::from(format!(
                        "cli-continue-{entry_id}"
                    )))
                    .ghost()
                    .xsmall()
                    .label("继续")
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.continue_entry(entry_id, cx);
                        },
                    )),
                )
                .child(div().text_xs().text_color(muted_fg).child(hint.clone()))
                .into_any_element()
        }
        TranscriptRow::Header { command, meta } => div()
            .h(px(ROW_H))
            .w_full()
            .px(px(12.0))
            .whitespace_nowrap()
            .text_xs()
            .text_color(muted_fg)
            .font_family("monospace")
            .child(SharedString::from(format!("{command} · {meta}")))
            .into_any_element(),
        TranscriptRow::Body { line, tone } => {
            let color = match tone {
                LineTone::Normal => fg,
                LineTone::Muted => muted_fg,
                LineTone::Accent => accent,
                LineTone::Error => gpui::red(),
            };
            div()
                .h(px(ROW_H))
                .w_full()
                .px(px(12.0))
                .whitespace_nowrap()
                .text_sm()
                .text_color(color)
                .font_family("monospace")
                .child(line.clone())
                .into_any_element()
        }
        TranscriptRow::Spacer => div().h(px(ROW_H)).w_full().into_any_element(),
    }
}
