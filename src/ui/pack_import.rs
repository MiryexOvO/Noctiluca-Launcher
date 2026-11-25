use super::*;
use crate::packs::Preview;
use ely_gpui_component::{buttons::ButtonVariant, forms::Checkbox, primitives::Tooltip};
use std::collections::HashSet;
use std::path::Path;

pub(super) struct Draft {
    pack: Arc<Preview>,
    path: PathBuf,
    optional: HashSet<usize>,
    supplied: HashSet<usize>,
    failed_file: Option<usize>,
    missing_page: usize,
    optional_page: usize,
    optional_open: bool,
}

impl Draft {
    fn selected(&self, i: usize) -> bool {
        !self.pack.files[i].optional() || self.optional.contains(&i)
    }
    fn missing(&self, i: usize) -> bool {
        self.selected(i) && !self.supplied.contains(&i) && self.pack.files[i].automatic.is_none()
    }
}

impl Home {
    pub(super) fn drop_pack(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        if self.cancel.is_some() || self.logging_in || self.copying {
            return;
        }
        if let [path] = paths
            && path.is_file()
            && path
                .extension()
                .and_then(|v| v.to_str())
                .is_some_and(|v| v.eq_ignore_ascii_case("zip") || v.eq_ignore_ascii_case("mrpack"))
        {
            self.install_pack(hub::Pack::Dropped(path.clone()), cx);
        } else {
            self.message(
                Err(anyhow::anyhow!("请每次拖入一个 .mrpack 或 .zip 整合包")),
                cx,
            );
        }
    }

    pub(super) fn preview_ready(
        &mut self,
        result: Result<(PathBuf, Box<Preview>)>,
        dropped: bool,
        cx: &mut Context<Self>,
    ) {
        self.cancel = None;
        match result {
            Ok((path, pack)) => {
                let selected = if let Some(item) = &pack.target {
                    crate::content::previous_options(&self.root, item, &pack)
                } else {
                    HashSet::new()
                };
                self.draft = Some(Draft {
                    pack: Arc::from(pack),
                    path,
                    optional: selected,
                    supplied: HashSet::new(),
                    failed_file: None,
                    missing_page: 0,
                    optional_page: 0,
                    optional_open: false,
                });
                self.status = "整合包预览已就绪".into();
                self.failed = false;
                let draft = self.draft.as_ref().unwrap();
                if crate::preferences::current().quick == 1
                    && (dropped || draft.pack.upstream.is_some())
                    && draft.pack.target.is_none()
                    && draft.pack.metadata_error.is_none()
                    && draft.pack.optional_error.is_none()
                    && draft.pack.optional_count == 0
                    && !(0..draft.pack.files.len()).any(|i| draft.missing(i))
                {
                    self.confirm_pack(false, cx);
                } else if dropped
                    && draft.pack.metadata_error.is_none()
                    && draft
                        .pack
                        .files
                        .iter()
                        .any(|f| !f.optional() && f.automatic.is_some())
                {
                    let pack = draft.pack.clone();
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.cancel = Some(cancel.clone());
                    self.status = "正在预下载整合包必需文件".into();
                    let (tx, rx) = async_channel::unbounded();
                    std::thread::spawn(move || {
                        let result = pack.prefetch(&cancel, &|s| {
                            let _ = tx.try_send(Update::Status(s));
                        });
                        let _ = tx.try_send(Update::PackPrefetched(result));
                    });
                    self.listen(rx, cx);
                }
            }
            Err(e) => self.message(Err(e), cx),
        }
    }

    fn confirm_pack(&mut self, launch: bool, cx: &mut Context<Self>) {
        if self.cancel.is_some() || self.logging_in || self.copying {
            return;
        }
        let Some(draft) = &self.draft else { return };
        if draft.pack.metadata_error.is_some()
            || (0..draft.pack.files.len()).any(|i| draft.missing(i))
        {
            return;
        }
        let pack = draft.pack.clone();
        let optional = draft.optional.clone();
        let root = self.root.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.failed = false;
        self.status = "正在安装整合包文件".into();
        let (tx, rx) = async_channel::unbounded();
        std::thread::spawn(move || {
            let report = |s| {
                let _ = tx.try_send(Update::Status(s));
            };
            let result = (|| {
                let prepared = if let Some(current) = &pack.target {
                    let mut requested = pack.instance.clone();
                    requested.java_path = current.java_path.clone();
                    Some(game::stage_loader(&root, &requested, &cancel, &report)?)
                } else {
                    None
                };
                let item = if let Some(pin) = &prepared {
                    pack.install_prepared(&root, &optional, &cancel, report, pin.as_deref())?
                } else {
                    pack.install(&root, &optional, &cancel, report)?
                };
                let _ = tx.try_send(Update::Imported(Box::new(item.clone())));
                if prepared.is_some() {
                    Ok(())
                } else {
                    game::play(&root, &item, None, true, &cancel, report)
                }
            })();
            let _ = tx.try_send(Update::PackFinished(
                result,
                launch.then(|| pack.target.as_ref().unwrap_or(&pack.instance).id.clone()),
            ));
        });
        self.listen(rx, cx);
    }

    pub(super) fn pack_finished(
        &mut self,
        result: Result<()>,
        launch: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.cancel = None;
        match result {
            Err(err) => {
                if let Some(draft) = &mut self.draft
                    && let Some(file) = err.downcast_ref::<crate::download::FileFailure>()
                {
                    draft.failed_file = draft.pack.files.iter().position(|f| f.path == file.label);
                    draft.missing_page = 0;
                }
                self.message(Err(err), cx);
            }
            Ok(()) => {
                self.failed = false;
                self.status = "整合包、游戏与加载器已安装".into();
                if let Some(id) = launch
                    && let Some(i) = self.instances.iter().position(|item| item.id == id)
                {
                    self.show_instance(i, cx);
                    self.play(false, cx);
                }
            }
        }
    }

    fn pick_dependency(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(draft) = &self.draft else {
            return;
        };
        let pack = draft.pack.clone();
        let pick = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择与整合包清单匹配的文件".into()),
        });
        cx.spawn(async move |view, cx| {
            let choice = pick.await;
            let _ = view.update(cx, |home, cx| {
                if home.cancel.is_some()
                    || !home
                        .draft
                        .as_ref()
                        .is_some_and(|d| Arc::ptr_eq(&pack, &d.pack))
                {
                    return;
                }
                match choice {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            home.supply_file(pack, i, path, cx);
                        }
                    }
                    Ok(Err(err)) => home.message(Err(err), cx),
                    _ => {}
                }
            });
        })
        .detach();
    }

    fn supply_file(&mut self, pack: Arc<Preview>, i: usize, path: PathBuf, cx: &mut Context<Self>) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.status = format!("正在校验 {}", pack.files[i].path);
        self.failed = false;
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { pack.supply(i, &path, &cancel) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |home, cx| {
                home.cancel = None;
                match result {
                    Ok(()) => {
                        if let Some(draft) = &mut home.draft {
                            draft.supplied.insert(i);
                            if draft.failed_file == Some(i) {
                                draft.failed_file = None;
                            }
                            home.status = format!("{} 校验通过", draft.pack.files[i].path);
                        }
                    }
                    Err(e) => home.message(Err(e), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn pack_page(&self, frame: motion::Frame, cx: &Context<Self>) -> impl IntoElement {
        let busy = self.cancel.is_some();
        let mut body = div().flex().flex_col().gap_4();
        let mut footer = div()
            .flex_none()
            .flex()
            .items_center()
            .justify_end()
            .flex_wrap()
            .gap_2()
            .border_t_1()
            .border_color(rgb(LINE))
            .px_6()
            .py_3();
        if let Some(draft) = &self.draft {
            let pack = &draft.pack;
            let item = &pack.instance;
            let source = pack.format;
            let loader = match &item.loader_version {
                Some(version) => format!("{} {version}", item.loader.label()),
                None => item.loader.label().to_owned(),
            };
            body = body
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .child(div().min_w_0().flex_1().child(heading(&item.name)))
                        .child(
                            Button::new("choose-another-pack", "选择其他整合包")
                                .variant(ButtonVariant::Ghost)
                                .disabled(busy)
                                .on_click(cx.listener(|h, _, _, cx| h.pick_pack(cx))),
                        ),
                )
                .child(kicker(&format!(
                    "{}{source}  ·  Minecraft {}  ·  {loader}  ·  {} 个必需文件",
                    pack.target
                        .as_ref()
                        .map(|t| format!("更新「{}」 · ", t.name))
                        .unwrap_or_default(),
                    item.version,
                    pack.required_count
                )));
            if let Some(error) = &pack.metadata_error {
                body = body.child(panel("无法读取依赖文件信息").child(error.clone()));
                let path = draft.path.clone();
                let origin = pack.upstream.clone();
                let target = pack.target.clone();
                footer = footer.child(
                    Button::new("retry-manifest", "重新读取清单")
                        .primary()
                        .disabled(busy)
                        .on_click(cx.listener(move |h, _, _, cx| {
                            h.install_pack(
                                hub::Pack::Retry(path.clone(), origin.clone(), target.clone()),
                                cx,
                            )
                        })),
                );
            } else {
                let missing: Vec<_> = (0..pack.files.len())
                    .filter(|&i| draft.missing(i))
                    .collect();
                let (visible_missing, missing_pager) =
                    self.file_page(&missing, false, draft.missing_page, cx);
                if !missing.is_empty() {
                    let mut list = panel(&format!("还需补齐 {} 个文件", missing.len()));
                    for &i in visible_missing {
                        list = list.child(self.missing_file_row(
                            draft,
                            i,
                            missing.len() > 1,
                            busy,
                            cx,
                        ));
                    }
                    body = body.child(list.children(missing_pager));
                }
                if let Some(i) = draft.failed_file
                    && draft.selected(i)
                    && !draft.missing(i)
                    && !draft.supplied.contains(&i)
                {
                    body = body.child(
                        panel("下载失败，可重新安装或手动补齐")
                            .child(self.missing_file_row(draft, i, true, busy, cx)),
                    );
                }
                if let Some(error) = &pack.optional_error {
                    body = body.child(
                        panel("可选文件信息暂不可用，可继续安装必需内容").child(error.clone()),
                    );
                } else if pack.optional_count > 0 {
                    body = body.child(self.optional_files(draft, busy, cx));
                }
                if let Some(&i) = visible_missing.first() {
                    let path = pack.files[i].path.clone();
                    footer = footer
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .truncate()
                                .child(format!("待补齐  ·  {path}")),
                        )
                        .child(
                            Button::new("next-manual-file", "选择已下载文件")
                                .primary()
                                .disabled(busy)
                                .on_click(cx.listener(move |h, _, _, cx| h.pick_dependency(i, cx))),
                        );
                } else {
                    footer = footer
                        .child(
                            Button::new(
                                "install-pack-only",
                                if pack.target.is_some() {
                                    "备份并更新"
                                } else {
                                    "仅安装"
                                },
                            )
                            .disabled(busy)
                            .on_click(cx.listener(|h, _, _, cx| h.confirm_pack(false, cx))),
                        )
                        .child(
                            Button::new(
                                "install-and-launch",
                                if pack.target.is_some() {
                                    "更新并启动"
                                } else {
                                    "安装并启动"
                                },
                            )
                            .primary()
                            .disabled(busy)
                            .on_click(cx.listener(|h, _, _, cx| h.confirm_pack(true, cx))),
                        );
                }
            }
        } else {
            body = body.child(heading("导入整合包")).when(!busy, |el| {
                el.child(kicker(
                    "将一个 .mrpack 或 .zip 文件拖入窗口，或从本机选择。",
                ))
            });
            footer = footer.when(!busy, |el| {
                el.child(
                    Button::new("choose-pack", "选择整合包")
                        .primary()
                        .on_click(cx.listener(|h, _, _, cx| h.pick_pack(cx))),
                )
            });
        }
        if busy {
            footer = footer.child(Button::new("cancel-import", "取消当前操作").on_click(
                cx.listener(|h, _, _, cx| {
                    if let Some(cancel) = &h.cancel {
                        cancel.store(true, Ordering::Release);
                    }
                    cx.notify();
                }),
            ));
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("pack-preview-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_6()
                    .child(frame.section(body, 0)),
            )
            .child(footer)
    }

    fn optional_files(&self, draft: &Draft, busy: bool, cx: &Context<Self>) -> Div {
        let pack = &draft.pack;
        let toggle = Button::new(
            "toggle-optionals",
            format!(
                "可选文件  ·  已选 {} / {}",
                draft.optional.len(),
                pack.optional_count
            ),
        )
        .icon(if draft.optional_open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        })
        .variant(ButtonVariant::Ghost)
        .on_click(cx.listener(|h, _, _, cx| {
            if let Some(d) = &mut h.draft {
                d.optional_open = !d.optional_open;
            }
            cx.notify();
        }));
        let mut options = div()
            .rounded_lg()
            .bg(rgb(PANEL))
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().flex().child(toggle));
        if draft.optional_open {
            let optional: Vec<_> = pack
                .files
                .iter()
                .enumerate()
                .filter(|(_, f)| f.optional())
                .map(|(i, _)| i)
                .collect();
            let (visible, pager) = self.file_page(&optional, true, draft.optional_page, cx);
            for &i in visible {
                let label = pack.files[i].path.clone();
                let view = cx.weak_entity();
                options = options.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Checkbox::new(("optional-pack-file", i), draft.optional.contains(&i))
                                .label(label)
                                .disabled(busy)
                                .on_change(move |checked, _, cx| {
                                    let _ = view.update(cx, |h, cx| {
                                        if let Some(d) = &mut h.draft {
                                            if checked {
                                                d.optional.insert(i);
                                            } else {
                                                d.optional.remove(&i);
                                            }
                                            d.missing_page = 0;
                                        }
                                        cx.notify();
                                    });
                                }),
                        )
                        .when(draft.supplied.contains(&i), |row| {
                            row.child(kicker("已校验"))
                        }),
                );
            }
            options = options.children(pager);
        }
        options
    }

    fn file_page<'a>(
        &self,
        rows: &'a [usize],
        optional: bool,
        page: usize,
        cx: &Context<Self>,
    ) -> (&'a [usize], Option<Div>) {
        const PER_PAGE: usize = 8;
        let pages = rows.len().div_ceil(PER_PAGE);
        let page = page.min(pages.saturating_sub(1));
        let start = page * PER_PAGE;
        let visible = &rows[start..(start + PER_PAGE).min(rows.len())];
        let pager = (pages > 1).then(|| {
            let change = move |h: &mut Home, to: usize, cx: &mut Context<Home>| {
                if let Some(d) = &mut h.draft {
                    if optional {
                        d.optional_page = to;
                    } else {
                        d.missing_page = to;
                    }
                }
                cx.notify();
            };
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    Button::new(("pack-prev", usize::from(optional)), "上一页")
                        .disabled(page == 0)
                        .on_click(
                            cx.listener(move |h, _, _, cx| change(h, page.saturating_sub(1), cx)),
                        ),
                )
                .child(kicker(&format!("{} / {pages}", page + 1)))
                .child(
                    Button::new(("pack-next", usize::from(optional)), "下一页")
                        .disabled(page + 1 >= pages)
                        .on_click(cx.listener(move |h, _, _, cx| change(h, page + 1, cx))),
                )
        });
        (visible, pager)
    }

    fn missing_file_row(
        &self,
        draft: &Draft,
        i: usize,
        manual_action: bool,
        busy: bool,
        cx: &Context<Self>,
    ) -> Div {
        let file = &draft.pack.files[i];
        let mut row = div().flex().flex_wrap().items_center().gap_2().child(
            div()
                .id(("pack-file-name", i))
                .flex_1()
                .min_w(px(130.))
                .truncate()
                .tooltip(Tooltip::text(file.path.clone()))
                .child(file.path.clone()),
        );
        let (label, url) = if let Some(url) = file.website() {
            ("打开下载页", Some(url))
        } else {
            let filename = Path::new(&file.path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            (
                "在 Modrinth 查找",
                url::Url::parse_with_params(
                    "https://modrinth.com/discover/mods",
                    &[("q", filename.as_ref())],
                )
                .ok()
                .map(Into::into),
            )
        };
        if let Some(url) = url {
            row = row.child(
                Button::new(("pack-site", i), label)
                    .variant(ButtonVariant::Link)
                    .on_click(cx.listener(move |h, _, _, cx| {
                        h.message(open::that_detached(&url).map_err(Into::into), cx)
                    })),
            );
        }
        if manual_action {
            row = row.child(
                Button::new(("pack-manual", i), "选择文件")
                    .disabled(busy)
                    .on_click(cx.listener(move |h, _, _, cx| h.pick_dependency(i, cx))),
            );
        }
        row
    }
}
