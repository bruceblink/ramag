use super::*;

impl MongoQueryTab {
    /// 解析并校验命令；高危操作先展示目标与风险，确认后才进入真正执行路径。
    pub fn request_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let text = self.editor.read(cx).value();
        if text.len() > ramag_ui::MAX_EDITOR_DRAFT_BYTES {
            self.result.update(cx, |panel, cx| {
                panel.set_error(
                    format!(
                        "MongoDB 命令超过 {} MiB 安全上限，无法运行；请拆分命令后重试",
                        ramag_ui::MAX_EDITOR_DRAFT_BYTES / 1024 / 1024
                    ),
                    cx,
                );
            });
            return;
        }
        let text = text.to_string();
        let cmd: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                self.result.update(cx, |p, cx| {
                    p.set_error(format!("JSON 解析失败：{e}"), cx);
                });
                return;
            }
        };
        if !cmd.is_object() {
            self.result.update(cx, |p, cx| {
                p.set_error("顶层 JSON 必须是对象".to_string(), cx);
            });
            return;
        }
        if let Some(reason) = dangerous_command_reason(&cmd) {
            let command_preview = if text.len() <= MAX_CONFIRM_PRETTY_BYTES {
                let pretty = json_pretty_bounded(&cmd, MAX_CONFIRM_PRETTY_BYTES)
                    .unwrap_or_else(|| text.clone());
                truncate_chars(&pretty, 1_000)
            } else {
                format!(
                    "{}\n\n（命令超过 {} KiB，仅展示原文前缀）",
                    truncate_chars(&text, 1_000),
                    MAX_CONFIRM_PRETTY_BYTES / 1024
                )
            };
            let description = format!(
                "连接：{}\n数据库：{}\n风险：{reason}\n\n命令：\n{command_preview}\n\n确认继续执行吗？",
                self.config.name, self.database
            );
            let confirmed_database = self.database.clone();
            let entity = cx.entity();
            ramag_ui::open_confirm(
                "执行 MongoDB 高危命令？",
                description,
                "执行",
                true,
                move |_window, app| {
                    entity.update(app, |this, cx| {
                        if this.database != confirmed_database
                            || this.editor.read(cx).value() != text
                        {
                            this.pending_notification = Some(
                                Notification::warning("数据库或命令已变更，已取消执行；请重新确认")
                                    .autohide(true),
                            );
                            cx.notify();
                            return;
                        }
                        this.run_parsed(text.clone(), cmd.clone(), cx)
                    });
                },
                window,
                cx,
            );
            return;
        }
        self.run_parsed(text, cmd, cx);
    }

    /// 真正执行已解析、已确认（如需要）的命令。
    pub(super) fn run_parsed(&mut self, text: String, cmd: Value, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let response_kind = command_response_kind(&cmd);
        let (effective_command, page_request) = if let Some(pager) = MongoPager::from_command(&cmd)
        {
            let page = match pager.command_for_page(0) {
                Ok(page) => page,
                Err(message) => {
                    self.result.update(cx, |panel, cx| {
                        panel.set_error(format!("MongoDB 分页初始化失败：{message}"), cx)
                    });
                    return;
                }
            };
            self.pager = Some(pager);
            (page.0, Some(page.1))
        } else {
            self.pager = None;
            (cmd.clone(), None)
        };
        self.execute_command(
            cmd,
            effective_command,
            response_kind,
            Some(text),
            page_request,
            cx,
        );
    }

    /// 加载相邻结果页，不改写编辑器或历史。
    pub(super) fn handle_page(&mut self, requested_page: usize, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let Some(pager) = self.pager.as_ref() else {
            return;
        };
        if !pager.accepts_adjacent_page(requested_page) {
            return;
        }
        let base_command = pager.base_command().clone();
        let (effective_command, page_request) = match pager.command_for_page(requested_page) {
            Ok(page) => page,
            Err(message) => {
                self.pending_notification = Some(
                    Notification::error(format!("加载 MongoDB 分页失败：{message}")).autohide(true),
                );
                cx.notify();
                return;
            }
        };
        let response_kind = command_response_kind(&base_command);
        self.execute_command(
            base_command,
            effective_command,
            response_kind,
            None,
            Some(page_request),
            cx,
        );
    }

    /// 执行原始命令或分页命令。
    pub(super) fn execute_command(
        &mut self,
        base_command: Value,
        effective_command: Value,
        response_kind: CommandResponseKind,
        history_text: Option<String>,
        page_request: Option<PageRequest>,
        cx: &mut Context<Self>,
    ) {
        // 同步命令目标与当前库，避免写操作仍使用标签初始库。
        let target = extract_collection(&base_command);
        self.collection = target.clone();
        let db_now = self.database.clone();
        self.result.update(cx, |p, _| {
            p.set_database(db_now);
            p.set_target_collection(target);
        });

        let svc = self.service.clone();
        let conf = self.config.clone();
        let db = self.database.clone();
        self.running = true;
        // 代际推进 + 记录本次运行的 db（回包时比对，防运行期间切库导致串台）
        self.run_seq = self.run_seq.wrapping_add(1);
        let request_seq = self.run_seq;
        let request_db = self.database.clone();
        // 生产只读拦截（Forbidden）时恢复用：set_running 会清掉原错误文案
        let prev_error = self.result.read(cx).error.clone();
        self.result.update(cx, |p, cx| p.set_running(cx));
        let result_handle = self.result.clone();

        let task = cx.spawn(async move |this, cx| {
            let start = Instant::now();
            let outcome = svc.run_command(&conf, &db, effective_command).await;
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let (qr, page_has_more): (ramag_domain::error::Result<MongoQueryResult>, Option<bool>) =
                match outcome {
                    Ok(resp) => {
                        let mut result =
                            parse_run_command_response(resp, elapsed_ms, response_kind);
                        let has_more =
                            page_request.map(|request| finish_page(&mut result, request));
                        (Ok(result), has_more)
                    }
                    Err(e) => (Err(e), None),
                };
            // 写历史在同 task 顺序执行，避免 DomainError 不实现 Clone 的借用难题
            if let Some(command_text) = history_text {
                svc.append_history(&conf, command_text, &qr).await;
            }

            let _ = this.update(cx, |this, cx| {
                // 请求身份校验：切库 / 重新运行后旧回包不得覆盖新上下文的结果
                if this.run_seq != request_seq || this.database != request_db {
                    // 仅当自己仍是最新在途请求时才复位忙碌态（含结果区，否则「执行中」
                    // 永久卡在界面上）；已有更新请求在途则一概不动，避免误清新查询状态
                    if this.run_seq == request_seq {
                        this.running = false;
                        result_handle.update(cx, |p, cx| {
                            p.set_error("查询上下文已切换，本次结果已丢弃；请重新运行".into(), cx);
                        });
                    }
                    return;
                }
                this.running = false;
                this.current_task = None;
                match qr {
                    Ok(r) => {
                        let pagination =
                            page_request
                                .zip(page_has_more)
                                .and_then(|(request, has_more)| {
                                    let displayed = r.documents.len();
                                    this.pager.as_mut().map(|pager| {
                                        pager.finish_request(request, displayed, has_more);
                                        MongoResultPagination {
                                            page: request.page,
                                            page_size: request.page_size,
                                            has_more: pager.has_more,
                                        }
                                    })
                                });
                        info!(
                            db = %this.database,
                            docs = r.documents.len(),
                            ms = r.elapsed_ms,
                            "command completed"
                        );
                        result_handle.update(cx, |panel, cx| {
                            panel.set_result(r, cx);
                            if panel.result.is_some() {
                                panel.set_pagination(pagination, cx);
                            }
                        });
                    }
                    Err(e) => {
                        warn!(error = %e, "command failed");
                        // 生产模式只读拦截：弹 toast 并复位忙碌态（旧结果 / 旧错误原样恢复，
                        // 否则结果区永久停在"执行中"）；其余错误仍进结果区便于排查
                        if matches!(e, DomainError::Forbidden(_)) {
                            this.pending_notification =
                                Some(Notification::warning(e.to_string()).autohide(true));
                            result_handle.update(cx, |p, cx| p.restore_idle(prev_error, cx));
                        } else {
                            result_handle.update(cx, |p, cx| p.set_error(e.to_string(), cx));
                        }
                    }
                }
                cx.notify();
            });
        });
        self.current_task = Some(task);
    }

    /// 关闭标签前停止等待当前命令。MongoDB 暂无可靠 killOp 句柄，因此只保证客户端任务退出、
    /// 结果不再回写；服务器端命令仍由服务端超时/完成机制收尾。
    pub fn cancel_if_running(&mut self, cx: &mut Context<Self>) {
        if self.current_task.take().is_some() || self.running {
            self.run_seq = self.run_seq.wrapping_add(1);
            self.running = false;
            self.result.update(cx, |panel, cx| {
                panel.set_error("已停止等待该命令；服务器端操作可能仍在收尾".into(), cx);
            });
            cx.notify();
        }
    }

    pub fn focus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            state.focus(window, cx);
        });
    }

    pub fn format_json(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.formatting {
            self.pending_notification =
                Some(Notification::info("JSON 格式化正在进行").autohide(true));
            cx.notify();
            return;
        }
        let text = self.editor.read(cx).value();
        if text.len() > ramag_ui::MAX_EDITOR_DRAFT_BYTES {
            self.result.update(cx, |panel, cx| {
                panel.set_error(
                    format!(
                        "MongoDB 命令超过 {} MiB 安全上限，无法格式化；请拆分命令后重试",
                        ramag_ui::MAX_EDITOR_DRAFT_BYTES / 1024 / 1024
                    ),
                    cx,
                );
            });
            return;
        }
        if text.trim().is_empty() {
            return;
        }
        self.formatting = true;
        cx.notify();
        let source_text = text.clone();
        cx.spawn_in(window, async move |this, async_cx| {
            let formatted = ramag_app::run_blocking(move || {
                let parsed: Value = serde_json::from_str(&text).map_err(|error| {
                    DomainError::InvalidConfig(format!("格式化失败（JSON 无效）：{error}"))
                })?;
                json_pretty_bounded(&parsed, MAX_MONGO_INTERACTIVE_INPUT_BYTES).ok_or_else(|| {
                    DomainError::InvalidConfig(format!(
                        "格式化结果超过 {} MiB 安全上限",
                        MAX_MONGO_INTERACTIVE_INPUT_BYTES / 1024 / 1024
                    ))
                })
            })
            .await;
            let _ = this.update_in(async_cx, move |this, window, cx| {
                this.formatting = false;
                if this.editor.read(cx).value() != source_text {
                    this.pending_notification = Some(
                        Notification::warning("JSON 已在格式化期间发生变化，未覆盖新内容")
                            .autohide(true),
                    );
                    cx.notify();
                    return;
                }
                match formatted {
                    Ok(pretty) if pretty.len() > MAX_MONGO_INTERACTIVE_INPUT_BYTES => {
                        this.pending_notification = Some(
                            Notification::error(format!(
                                "格式化结果超过 {} MiB 安全上限，已保留原命令",
                                MAX_MONGO_INTERACTIVE_INPUT_BYTES / 1024 / 1024
                            ))
                            .autohide(true),
                        );
                    }
                    Ok(pretty) if pretty != source_text => {
                        this.editor.update(cx, |state, cx| {
                            state.set_value(pretty, window, cx);
                        });
                        cx.emit(MongoQueryTabEvent::DraftChanged);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        this.result.update(cx, |panel, cx| {
                            panel.set_error(error.to_string(), cx);
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
