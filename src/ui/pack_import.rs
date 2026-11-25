use super::*;
use crate::packs::Preview;
use ely_gpui_component::forms::Checkbox;
use std::collections::HashSet;
use std::path::Path;

pub(super) struct Draft {
    pack: Arc<Preview>,
    path: PathBuf,
    optional: HashSet<usize>,
    supplied: HashSet<usize>,
    failed_file: Option<usize>,
    page: usize,
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
            self.install_pack(hub::Pack::Local(path.clone()), cx);
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
        cx: &mut Context<Self>,
    ) {
        self.cancel = None;
        match result {
            Ok((path, pack)) => {
                self.draft = Some(Draft {
                    pack: Arc::from(pack),
                    path,
                    optional: HashSet::new(),
                    supplied: HashSet::new(),
                    failed_file: None,
                    page: 0,
                });
                self.status = "整合包预览已就绪".into();
                self.failed = false;
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
                let item = pack.install(&root, &optional, &cancel, report)?;
                let _ = tx.try_send(Update::Imported(Box::new(item.clone())));
                game::play(&root, &item, None, true, &cancel, report)
            })();
            let _ = tx.try_send(Update::PackFinished(
                result,
                launch.then(|| pack.instance.id.clone()),
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
                    draft.page = 0;
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

    pub(super) fn pack_page(&self, cx: &Context<Self>) -> impl IntoElement {
        let busy = self.cancel.is_some();
        let mut page = div().flex().flex_col().gap_4().child(heading("导入整合包"));
        if busy {
            page = page.child(
                Button::new("cancel-import", "取消当前操作").on_click(cx.listener(
                    |home, _, _, cx| {
                        if let Some(cancel) = &home.cancel {
                            cancel.store(true, Ordering::Release);
                        }
                        cx.notify();
                    },
                )),
            );
        }
        let Some(draft) = &self.draft else {
            return page.when(!busy, |el| {
                el.child(kicker(
                    "将一个 .mrpack 或 .zip 文件拖入窗口，或从本机选择。",
                ))
                .child(
                    Button::new("choose-pack", "选择整合包")
                        .on_click(cx.listener(|h, _, _, cx| h.pick_pack(cx))),
                )
            });
        };
        let pack = &draft.pack;
        let item = &pack.instance;
        let source = if pack.curse { "CurseForge" } else { "Modrinth" };
        page = page.child(
            panel(source)
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(item.name.clone()),
                )
                .child(format!(
                    "Minecraft {}    /    {} {}",
                    item.version,
                    item.loader.label(),
                    item.loader_version.as_deref().unwrap_or("")
                ))
                .child(kicker(&format!(
                    "必需文件 {} 个    ·    可选文件已选 {} / {}",
                    pack.required_count,
                    draft.optional.len(),
                    pack.optional_count
                ))),
        );
        if let Some(error) = &pack.optional_error {
            page =
                page.child(panel("可选文件信息暂不可用，可继续安装必需内容").child(error.clone()));
        }
        let mut actions = div().flex().flex_wrap().gap_2();
        if let Some(error) = &pack.metadata_error {
            page = page.child(panel("无法读取依赖文件信息").child(error.clone()));
            let path = draft.path.clone();
            actions = actions.child(
                Button::new("retry-manifest", "重新读取清单")
                    .disabled(busy)
                    .on_click(cx.listener(move |h, _, _, cx| {
                        h.install_pack(hub::Pack::Local(path.clone()), cx)
                    })),
            );
        } else {
            let missing = (0..pack.files.len()).filter(|&i| draft.missing(i)).count();
            let mut rows: Vec<_> = (0..pack.files.len())
                .filter(|&i| {
                    pack.files[i].optional()
                        || draft.missing(i)
                        || draft.supplied.contains(&i)
                        || draft.failed_file == Some(i)
                })
                .collect();
            rows.sort_by_key(|&i| (draft.failed_file != Some(i), !draft.missing(i)));
            let pages = rows.len().div_ceil(8);
            let current = draft.page.min(pages.saturating_sub(1));
            if !rows.is_empty() {
                let mut list = panel(&format!("文件选择    ·    待补齐 {missing} 个"));
                for &i in rows.iter().skip(current * 8).take(8) {
                    list = list.child(self.dependency_row(draft, i, busy, cx));
                }
                if pages > 1 {
                    list = list.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("previous-pack-page", "上一页")
                                    .disabled(current == 0)
                                    .on_click(cx.listener(move |h, _, _, cx| {
                                        if let Some(d) = &mut h.draft {
                                            d.page = current.saturating_sub(1);
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(kicker(&format!("{} / {pages}", current + 1)))
                            .child(
                                Button::new("next-pack-page", "下一页")
                                    .disabled(current + 1 >= pages)
                                    .on_click(cx.listener(move |h, _, _, cx| {
                                        if let Some(d) = &mut h.draft {
                                            d.page = current + 1;
                                        }
                                        cx.notify();
                                    })),
                            ),
                    );
                }
                page = page.child(list);
            }
            actions = actions
                .child(
                    Button::new("install-and-launch", "安装并启动")
                        .primary()
                        .disabled(busy || missing > 0)
                        .on_click(cx.listener(|h, _, _, cx| h.confirm_pack(true, cx))),
                )
                .child(
                    Button::new("install-pack-only", "仅安装")
                        .disabled(busy || missing > 0)
                        .on_click(cx.listener(|h, _, _, cx| h.confirm_pack(false, cx))),
                );
        }
        page.child(
            actions.child(
                Button::new("choose-another-pack", "选择其他整合包")
                    .disabled(busy)
                    .on_click(cx.listener(|h, _, _, cx| h.pick_pack(cx))),
            ),
        )
    }

    fn dependency_row(&self, draft: &Draft, i: usize, busy: bool, cx: &Context<Self>) -> Div {
        let file = &draft.pack.files[i];
        let view = cx.weak_entity();
        let mut row = div().flex().flex_wrap().items_center().gap_2();
        if file.optional() {
            row = row.child(
                Checkbox::new(("optional-pack-file", i), draft.optional.contains(&i))
                    .label(file.path.clone())
                    .disabled(busy)
                    .on_change(move |checked, _, cx| {
                        let _ = view.update(cx, |h, cx| {
                            if let Some(d) = &mut h.draft {
                                if checked {
                                    d.optional.insert(i);
                                } else {
                                    d.optional.remove(&i);
                                }
                            }
                            cx.notify();
                        });
                    }),
            );
        } else {
            row = row.child(file.path.clone());
        }
        if draft.supplied.contains(&i) {
            row = row.child(kicker("已校验"));
        } else if draft.missing(i) || draft.failed_file == Some(i) {
            row = row.child(kicker(if draft.failed_file == Some(i) {
                "下载失败，可重试安装或手动补齐"
            } else {
                "需手动补齐"
            }));
            if let Some(url) = file.website() {
                row = row.child(Button::new(("pack-website", i), "打开下载页").on_click(
                    cx.listener(move |h, _, _, cx| {
                        h.message(open::that_detached(&url).map_err(Into::into), cx)
                    }),
                ));
            } else {
                let filename = Path::new(&file.path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                if let Ok(url) = url::Url::parse_with_params(
                    "https://modrinth.com/discover/mods",
                    &[("q", filename.as_ref())],
                ) {
                    row = row.child(
                        Button::new(("pack-search", i), "在 Modrinth 查找").on_click(cx.listener(
                            move |h, _, _, cx| {
                                h.message(open::that_detached(url.as_str()).map_err(Into::into), cx)
                            },
                        )),
                    );
                }
            }
            row = row.child(
                Button::new(("pack-manual", i), "选择已下载文件")
                    .disabled(busy)
                    .on_click(cx.listener(move |h, _, _, cx| h.pick_dependency(i, cx))),
            );
        }
        row
    }
}
