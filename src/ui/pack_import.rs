mod review;
use super::*;
use crate::packs::Preview;
use ely_gpui_component::{buttons::ButtonVariant, forms::Checkbox, primitives::Tooltip};
use std::collections::HashSet;
use std::path::Path;

enum FileSelection {
    One(usize),
    Many,
    Folder,
}

pub(super) struct Draft {
    queue_drops: bool,
    queued_paths: Vec<PathBuf>,
    pub(super) pack: Arc<Preview>,
    review: Option<Arc<crate::content::review::Review>>,
    review_error: Option<String>,
    review_revision: u64,
    detail_open: bool,
    choices: crate::content::review::Choices,
    pub(super) optional: HashSet<usize>,
    supplied: HashSet<usize>,
    failed_file: Option<usize>,
    missing_page: usize,
    optional_page: usize,
    optional_open: bool,
}

impl Draft {
    pub(super) fn selected(&self, i: usize) -> bool {
        !self.pack.files[i].optional() || self.optional.contains(&i)
    }
    pub(super) fn missing(&self, i: usize) -> bool {
        self.selected(i) && !self.supplied.contains(&i) && self.pack.files[i].automatic.is_none()
    }
}

impl Home {
    pub(super) fn drop_pack(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        if self.page == Page::Import
            && let Some(d) = &mut self.draft
            && d.queue_drops
        {
            d.queued_paths.extend_from_slice(paths);
            self.status = "已收到拖入的文件，当前操作结束后会继续处理".into();
            cx.notify();
            return;
        }
        if self.cancel.is_some() || self.logging_in || self.copying {
            return;
        }
        let archive = match paths {
            [path] if path.is_file() => path.extension().and_then(|ext| ext.to_str()),
            _ => None,
        };
        let new_mrpack = archive.is_some_and(|ext| ext.eq_ignore_ascii_case("mrpack"));
        if self.page == Page::Import && self.draft.is_some() && !new_mrpack {
            self.match_files(paths.to_vec(), true, cx);
            return;
        }
        if !new_mrpack
            && matches!(
                self.page,
                Page::Detail | Page::Files | Page::Mods | Page::LocalResources
            )
            && self.instances.get(self.selected).is_some()
        {
            self.inspect_local_drop(paths.to_vec(), cx);
            return;
        }
        if new_mrpack || archive.is_some_and(|ext| ext.eq_ignore_ascii_case("zip")) {
            self.install_pack(hub::Pack::Dropped(paths[0].clone()), cx);
        } else {
            self.message(
                Err(anyhow::anyhow!("请每次拖入一个 .mrpack 或 .zip 整合包")),
                cx,
            );
        }
    }

    pub(super) fn preview_ready(
        &mut self,
        result: Result<Box<Preview>>,
        dropped: bool,
        cx: &mut Context<Self>,
    ) {
        let paused = self
            .cancel
            .take()
            .is_some_and(|c| c.load(Ordering::Relaxed));
        if paused {
            self.failed = false;
            self.status = "整合包任务已暂停，可以从下载任务继续".into();
            cx.notify();
            return;
        }
        match result {
            Ok(pack) => {
                let record = pack.job.as_ref().unwrap().record();
                let selected = if let Some(paths) = &record.optional {
                    pack.files
                        .iter()
                        .enumerate()
                        .filter_map(|(i, f)| (f.optional() && paths.contains(&f.path)).then_some(i))
                        .collect()
                } else if let Some(item) = &pack.target {
                    crate::content::previous_options(&self.root, item, &pack)
                } else {
                    HashSet::new()
                };
                let supplied = pack.cached();
                let failed_file = record.failed.as_ref().and_then(|p| {
                    pack.files
                        .iter()
                        .position(|f| &f.path == p)
                        .filter(|i| !supplied.contains(i))
                });
                self.draft = Some(Draft {
                    queue_drops: false,
                    queued_paths: Vec::new(),
                    pack: Arc::from(pack),
                    review: None,
                    review_error: None,
                    review_revision: 0,
                    detail_open: false,
                    choices: record.choices,
                    optional: selected,
                    supplied,
                    failed_file,
                    missing_page: 0,
                    optional_page: 0,
                    optional_open: false,
                });
                self.reload_jobs(cx);
                self.refresh_review(cx);
                self.status = "整合包预览已就绪".into();
                self.failed = false;
                let draft = self.draft.as_ref().unwrap();
                if self.tasks.closing {
                    cx.notify();
                    return;
                }
                let quick = crate::preferences::current().quick == 1
                    && (dropped || draft.pack.upstream.is_some())
                    && draft.pack.optional_count == 0;
                if (record.confirmed || quick)
                    && draft.failed_file.is_none()
                    && draft.pack.target.is_none()
                    && draft.pack.metadata_error.is_none()
                    && draft.pack.optional_error.is_none()
                    && !(0..draft.pack.files.len()).any(|i| draft.missing(i))
                {
                    self.confirm_pack(record.confirmed && record.launch, cx);
                } else if (dropped || record.prefetch)
                    && draft.failed_file.is_none()
                    && draft.pack.metadata_error.is_none()
                    && draft
                        .pack
                        .files
                        .iter()
                        .any(|f| !f.optional() && f.automatic.is_some())
                {
                    let pack = draft.pack.clone();
                    self.draft.as_mut().unwrap().queue_drops = true;
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.tasks.track(&cancel);
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
        if draft.pack.target.is_some() && draft.review.is_none() {
            return;
        }
        if let Err(e) = draft.persist_options() {
            self.message(Err(e), cx);
            return;
        }
        if let Err(e) = draft.pack.job.as_ref().unwrap().change(|r| {
            r.confirmed = true;
            r.launch = launch;
        }) {
            self.message(Err(e), cx);
            return;
        }
        let review = draft.review.clone();
        let choices = draft.choices.clone();
        let pack = draft.pack.clone();
        let optional = draft.optional.clone();
        let root = self.root.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.tasks.track(&cancel);
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
                let item = pack.install_prepared(
                    &root,
                    &optional,
                    &cancel,
                    report,
                    prepared.as_ref().and_then(|p| p.as_deref()),
                    review.as_ref().map(|r| (r.as_ref(), &choices)),
                )?;
                let job = pack.job.as_ref().unwrap();
                job.change(|r| r.committed = true)?;
                let _ = tx.try_send(Update::Imported(Box::new(item.clone())));
                if prepared.is_none() {
                    game::play(&root, &item, None, true, &cancel, report)?;
                }
                job.finish()
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
        let paused = self
            .cancel
            .take()
            .is_some_and(|c| c.load(Ordering::Relaxed));
        if paused && result.is_err() {
            self.failed = false;
            self.status = "整合包任务已暂停，下次可以继续安装".into();
            cx.notify();
            return;
        }
        match result {
            Err(err) => {
                if let Some(draft) = &mut self.draft
                    && let Some(file) = err.downcast_ref::<crate::download::FileFailure>()
                {
                    draft.failed_file = draft.pack.files.iter().position(|f| f.path == file.label);
                    draft.missing_page = 0;
                }
                if let Some(d) = &self.draft
                    && let Some(i) = d.failed_file
                    && let Some(j) = &d.pack.job
                {
                    let path = d.pack.files[i].path.clone();
                    if let Err(e) = j.change(|r| r.failed = Some(path)) {
                        self.message(Err(e), cx);
                        return;
                    }
                }
                self.message(Err(err), cx);
            }
            Ok(()) => {
                self.failed = false;
                self.draft = None;
                self.tasks.active = None;
                self.reload_jobs(cx);
                self.status = "整合包、游戏与加载器已安装".into();
                if !self.tasks.closing
                    && let Some(id) = launch
                    && let Some(i) = self.instances.iter().position(|item| item.id == id)
                {
                    self.show_instance(i, cx);
                    self.play(false, cx);
                }
            }
        }
    }

    pub(super) fn finish_prefetch(&mut self, result: Result<()>, cx: &mut Context<Self>) {
        let cancelled = self
            .cancel
            .take()
            .is_some_and(|c| c.load(Ordering::Relaxed));
        match result {
            Ok(()) => {
                self.failed = false;
                self.status = "可自动下载的必需文件已就绪".into();
            }
            Err(_) if cancelled => {
                self.failed = false;
                self.status = "整合包任务已暂停".into();
            }
            Err(e) => self.pack_finished(Err(e), None, cx),
        }
        self.next_drop(cancelled, cx);
    }

    fn next_drop(&mut self, cancelled: bool, cx: &mut Context<Self>) {
        if let Some(d) = &mut self.draft {
            d.queue_drops = false;
            let paths = std::mem::take(&mut d.queued_paths);
            if !cancelled && !paths.is_empty() && self.page == Page::Import {
                self.drop_pack(&paths, cx);
            }
        }
    }

    fn pick_files(&mut self, selection: FileSelection, cx: &mut Context<Self>) {
        let Some(draft) = &self.draft else {
            return;
        };
        let pack = draft.pack.clone();
        let folder = matches!(selection, FileSelection::Folder);
        let pick = cx.prompt_for_paths(PathPromptOptions {
            files: !folder,
            directories: folder,
            multiple: matches!(selection, FileSelection::Many),
            prompt: Some(
                match selection {
                    FileSelection::One(_) => "选择与整合包清单匹配的文件",
                    FileSelection::Many => "选择已下载的文件",
                    FileSelection::Folder => "选择要扫描的文件夹",
                }
                .into(),
            ),
        });
        cx.spawn(async move |view, cx| {
            let result = pick.await;
            let _ = view.update(cx, |home, cx| {
                if home.cancel.is_some()
                    || home.page != Page::Import
                    || !home
                        .draft
                        .as_ref()
                        .is_some_and(|d| Arc::ptr_eq(&d.pack, &pack))
                {
                    return;
                }
                match result {
                    Ok(Ok(Some(paths))) => match selection {
                        FileSelection::One(i) => {
                            if let Some(path) = paths.into_iter().next() {
                                home.supply_file(pack, i, path, cx);
                            }
                        }
                        _ => home.match_files(paths, false, cx),
                    },
                    Ok(Err(e)) => home.message(Err(e), cx),
                    _ => {}
                }
            });
        })
        .detach();
    }

    fn match_files(&mut self, paths: Vec<PathBuf>, dropped: bool, cx: &mut Context<Self>) {
        if self.cancel.is_some() || paths.is_empty() {
            return;
        }
        let Some(draft) = &mut self.draft else {
            return;
        };
        let pack = draft.pack.clone();
        let optional = draft.optional.clone();
        draft.queue_drops = true;
        let cancel = Arc::new(AtomicBool::new(false));
        self.tasks.track(&cancel);
        self.cancel = Some(cancel.clone());
        self.failed = false;
        self.status = "正在按整合包清单匹配本地文件…".into();
        let task = cx.background_executor().spawn(async move {
            let result = pack.supply_batch(&paths, &optional, &cancel);
            let cancelled = cancel.load(Ordering::Relaxed);
            let archive = match paths.as_slice() {
                [p] if dropped
                    && !cancelled
                    && result.matched.is_empty()
                    && p.extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
                    && crate::packs::is_pack_archive(p) =>
                {
                    Some(p.clone())
                }
                _ => None,
            };
            (pack, result, cancelled, archive)
        });
        cx.spawn(async move |view, cx| {
            let (pack, result, cancelled, archive) = task.await;
            let _ = view.update(cx, |home, cx| {
                // A cancellation can arrive after the worker finishes reading.
                let cancelled = cancelled
                    || home
                        .cancel
                        .as_ref()
                        .is_some_and(|c| c.load(Ordering::Relaxed));
                home.cancel = None;
                let Some(draft) = home.draft.as_mut().filter(|d| Arc::ptr_eq(&d.pack, &pack))
                else {
                    return;
                };
                if let Some(path) = archive
                    && !cancelled
                {
                    home.install_pack(hub::Pack::Dropped(path), cx);
                    return;
                }
                let count = result.matched.len();
                draft.supplied.extend(result.matched);
                if draft
                    .failed_file
                    .is_some_and(|i| draft.supplied.contains(&i))
                {
                    draft.failed_file = None;
                }
                draft.missing_page = 0;
                let pending = (0..pack.files.len())
                    .filter(|&i| {
                        draft.missing(i) || (draft.selected(i) && draft.failed_file == Some(i))
                    })
                    .count();
                home.status = format!(
                    "{}已匹配 {count} 个清单文件，还需手动补齐 {pending} 个",
                    if cancelled { "已取消扫描 · " } else { "" }
                );
                if let Some(e) = result.first_error {
                    home.status
                        .push_str(&format!(" · {} 项读取或校验失败：{e}", result.errors));
                }
                home.failed = result.errors > 0;
                home.next_drop(cancelled, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn supply_file(&mut self, pack: Arc<Preview>, i: usize, path: PathBuf, cx: &mut Context<Self>) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.tasks.track(&cancel);
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
            .border_color(cx.theme().colors.border)
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
                .child(kicker(
                    cx,
                    &format!(
                        "{}{source}  ·  Minecraft {}  ·  {loader}  ·  {} 个必需文件",
                        pack.target
                            .as_ref()
                            .map(|t| format!("更新「{}」 · ", t.name))
                            .unwrap_or_default(),
                        item.version,
                        pack.required_count
                    ),
                ));
            let estimate = pack.estimate(&draft.optional);
            let pending = match (estimate.unknown_count, estimate.pending_bytes) {
                (0, bytes) => format_bytes(bytes),
                (_, 0) => "待确认".to_owned(),
                (_, bytes) => format!("至少 {}", format_bytes(bytes)),
            };
            let mut transfer = panel(cx, "整合包依赖下载量").child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .child(format!(
                        "本次已缓存 {}（{} 个文件）",
                        format_bytes(estimate.cached_bytes),
                        estimate.cached_count
                    ))
                    .child(format!(
                        "还需下载 {pending}（{} 个文件）",
                        estimate.pending_count
                    )),
            );
            if estimate.unknown_count > 0 {
                transfer = transfer.child(kicker(
                    cx,
                    &format!("{} 个待获取文件的大小未知", estimate.unknown_count),
                ));
            }
            if estimate.pending_count > 0 && pack.metadata_error.is_none() {
                transfer = transfer
                    .child(kicker(
                        cx,
                        "把已下载的文件拖到这里，或扫描整个文件夹（含子文件夹）。",
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Button::new("supply-many", "选择多个文件")
                                    .disabled(busy)
                                    .on_click(cx.listener(|h, _, _, cx| {
                                        h.pick_files(FileSelection::Many, cx)
                                    })),
                            )
                            .child(
                                Button::new("supply-folder", "扫描文件夹")
                                    .disabled(busy)
                                    .on_click(cx.listener(|h, _, _, cx| {
                                        h.pick_files(FileSelection::Folder, cx)
                                    })),
                            ),
                    );
            }
            body = body.child(transfer.child(kicker(
                cx,
                "按当前选中的清单文件统计。基础游戏、加载器和 Java 在安装时单独检查。",
            )));
            if pack.target.is_some() {
                body = body.child(self.review_panel(draft, busy, cx));
            }
            if let Some(error) = &pack.metadata_error {
                body = body.child(panel(cx, "无法读取依赖文件信息").child(error.clone()));
                let id = pack.instance.id.clone();
                footer = footer.child(
                    Button::new("retry-manifest", "重新读取清单")
                        .primary()
                        .disabled(busy)
                        .on_click(cx.listener(move |h, _, _, cx| {
                            h.install_pack(hub::Pack::Resume(id.clone()), cx)
                        })),
                );
            } else {
                let missing: Vec<_> = (0..pack.files.len())
                    .filter(|&i| draft.missing(i))
                    .collect();
                let (visible_missing, missing_pager) =
                    self.file_page(&missing, false, draft.missing_page, cx);
                if !missing.is_empty() {
                    let mut list = panel(cx, &format!("还需补齐 {} 个文件", missing.len()));
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
                        panel(cx, "下载失败")
                            .child(
                                Button::new("retry-pack-file", "重试这个文件")
                                    .disabled(busy)
                                    .on_click(cx.listener(move |h, _, _, cx| h.retry_one(i, cx))),
                            )
                            .child(self.missing_file_row(draft, i, true, busy, cx)),
                    );
                }
                if let Some(error) = &pack.optional_error {
                    body = body.child(
                        panel(cx, "可选文件信息暂不可用，可继续安装必需内容").child(error.clone()),
                    );
                } else if pack.optional_count > 0 {
                    body = body.child(self.optional_files(draft, busy, cx));
                }
                if !missing.is_empty() {
                    footer = footer.child(kicker(
                        cx,
                        &format!("还需手动补齐 {} 个文件", missing.len()),
                    ));
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
                            .disabled(busy || (pack.target.is_some() && draft.review.is_none()))
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
                            .disabled(busy || (pack.target.is_some() && draft.review.is_none()))
                            .on_click(cx.listener(|h, _, _, cx| h.confirm_pack(true, cx))),
                        );
                }
            }
        } else {
            body = body.child(heading("导入整合包")).when(!busy, |el| {
                el.child(kicker(
                    cx,
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
            .bg(cx.theme().colors.surface)
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
                                        h.save_options(cx);
                                        h.refresh_review(cx);
                                        cx.notify();
                                    });
                                }),
                        )
                        .when(draft.supplied.contains(&i), |row| {
                            row.child(kicker(cx, "已校验"))
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
                .child(kicker(cx, &format!("{} / {pages}", page + 1)))
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
                    .on_click(
                        cx.listener(move |h, _, _, cx| h.pick_files(FileSelection::One(i), cx)),
                    ),
            );
        }
        row
    }
}
