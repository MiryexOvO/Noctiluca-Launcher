use super::*;
use crate::download::Downloads;
use crate::market::{self, Hit, Release, Source};

pub(super) enum Pack {
    Local(PathBuf),
    Online(Release),
}
pub(super) struct Hub {
    pub(super) packs: bool,
    loader: Loader,
    source: Source,
    mc: Entity<TextInput>,
    query: Entity<TextInput>,
    builds: Vec<String>,
    hits: Vec<Hit>,
    releases: Vec<Release>,
    project: Option<String>,
    result_mc: String,
    result_query: String,
    offset: usize,
    page: usize,
    pub(super) loading: bool,
}
impl Hub {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Home>) -> Self {
        Self {
            packs: false,
            loader: Loader::NeoForge,
            source: Source::Modrinth,
            mc: cx.new(|cx| {
                TextInput::new(window, cx)
                    .label("Minecraft 版本")
                    .placeholder("例如 1.21.1")
                    .max_len(80)
            }),
            query: cx.new(|cx| {
                TextInput::new(window, cx)
                    .label("整合包名称")
                    .placeholder("输入名称，或留空浏览热门整合包")
                    .max_len(100)
            }),
            builds: vec![],
            hits: vec![],
            releases: vec![],
            project: None,
            result_mc: String::new(),
            result_query: String::new(),
            offset: 0,
            page: 0,
            loading: false,
        }
    }
    fn clear(&mut self) {
        self.builds.clear();
        self.hits.clear();
        self.releases.clear();
        self.project = None;
        self.offset = 0;
        self.page = 0;
    }
}
enum Results {
    Builds(Vec<String>),
    Projects(Vec<Hit>),
    Files(String, Vec<Release>),
}
impl Home {
    pub(super) fn install_pack(&mut self, pack: Pack, cx: &mut Context<Self>) {
        if self.cancel.is_some() || self.copying || self.logging_in {
            return;
        }
        self.draft = None;
        let root = self.root.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.page = Page::Import;
        self.failed = false;
        self.status = "准备整合包".into();
        let (tx, rx) = async_channel::unbounded();
        std::thread::spawn(move || {
            let report = |s| {
                let _ = tx.try_send(Update::Status(s));
            };
            let result = (|| -> Result<_> {
                let path = match pack {
                    Pack::Local(p) => p,
                    Pack::Online(file) => {
                        file.archive(&root, &Downloads::new(&cancel, &report)?)?
                    }
                };
                let preview = crate::packs::preview(&root, &path)?;
                anyhow::ensure!(!cancel.load(Ordering::Relaxed), "已取消预览");
                Ok((path, Box::new(preview)))
            })();
            let _ = tx.try_send(Update::PackPreview(result));
        });
        self.listen(rx, cx);
    }
    fn lookup(
        &mut self,
        offset: usize,
        hit: Option<Hit>,
        page_request: bool,
        cx: &mut Context<Self>,
    ) {
        if self.hub.loading {
            return;
        }
        let h = &mut self.hub;
        let (mc, query) = if hit.is_some() || page_request {
            (h.result_mc.clone(), h.result_query.clone())
        } else {
            (
                h.mc.read(cx).text().trim().to_owned(),
                h.query.read(cx).text().trim().to_owned(),
            )
        };
        let (source, loader, packs) = (h.source, h.loader, h.packs);
        if !packs && !self.versions.is_empty() && !self.versions.iter().any(|v| v.id == mc) {
            self.message(
                Err(anyhow::anyhow!(
                    "请先选一个支持的 Minecraft 版本（1.17 起）"
                )),
                cx,
            );
            return;
        }
        if hit.is_none() {
            h.clear();
        }
        h.result_mc = mc.clone();
        h.result_query = query.clone();
        h.offset = offset;
        h.page = 0;
        h.loading = true;
        self.failed = false;
        self.status = "正在查询版本".into();
        let task = cx.background_executor().spawn(async move {
            if let Some(hit) = hit {
                return market::releases(source, loader, &mc, &hit.id)
                    .map(|v| Results::Files(hit.title, v));
            }
            if packs {
                market::search(source, loader, &mc, &query, offset).map(Results::Projects)
            } else {
                market::builds(loader, &mc).map(Results::Builds)
            }
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |home, cx| {
                let h = &mut home.hub;
                h.loading = false;
                let count = match result {
                    Ok(Results::Builds(v)) => {
                        h.builds = v;
                        h.builds.len()
                    }
                    Ok(Results::Projects(v)) => {
                        h.hits = v;
                        h.hits.len()
                    }
                    Ok(Results::Files(title, v)) => {
                        h.project = Some(title);
                        h.releases = v;
                        h.releases.len()
                    }
                    Err(e) => {
                        home.message(Err(e), cx);
                        return;
                    }
                };
                home.status = if count == 0 {
                    "没有找到符合条件且可供下载的版本".into()
                } else {
                    format!("找到 {count} 个结果")
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn install_build(&mut self, build: String, cx: &mut Context<Self>) -> Result<()> {
        let mc = self.hub.result_mc.clone();
        let loader = self.hub.loader;
        let mut item = Instance::new(format!("{} {} · {}", loader.label(), mc, build), mc, loader)?;
        item.loader_version = Some(build);
        library::save(&self.root, &item)?;
        self.instances.push(item);
        self.selected = self.instances.len() - 1;
        self.page = Page::Detail;
        self.play(true, cx);
        Ok(())
    }
    pub(super) fn hub_page(&self, cx: &Context<Self>) -> impl IntoElement {
        let h = &self.hub;
        let disabled = h.loading || self.cancel.is_some();
        let mut tabs = div().flex().gap_2();
        for (idx, (packs, label)) in [(false, "游戏下载"), (true, "整合包")]
            .into_iter()
            .enumerate()
        {
            tabs = tabs.child(
                Button::new(("market-tab", idx), label)
                    .when(h.packs == packs, |b| b.primary())
                    .disabled(disabled)
                    .on_click(cx.listener(move |home, _, _, cx| {
                        home.hub.clear();
                        home.hub.packs = packs;
                        home.status = "就绪".into();
                        home.failed = false;
                        cx.notify();
                    })),
            );
        }
        let mut loaders = div().flex().flex_wrap().gap_2();
        for (i, loader) in [Loader::NeoForge, Loader::Forge, Loader::Fabric]
            .into_iter()
            .enumerate()
        {
            loaders = loaders.child(
                Button::new(("market-loader", i), loader.label())
                    .when(h.loader == loader, |b| b.primary())
                    .disabled(disabled)
                    .on_click(cx.listener(move |home, _, _, cx| {
                        home.hub.loader = loader;
                        home.hub.clear();
                        cx.notify();
                    })),
            );
        }
        let mut filters = panel("加载器")
            .child(loaders)
            .child(kicker(if h.packs {
                "Minecraft 版本（可留空）"
            } else {
                "Minecraft 版本"
            }))
            .child(Input::new(&h.mc));
        let mut quick = div().flex().flex_wrap().gap_1();
        let mut versions = vec!["1.21.1".to_owned(), "1.20.1".to_owned()];
        if let Some(latest) = self.versions.iter().find(|v| v.kind == "release")
            && !versions.contains(&latest.id)
        {
            versions.insert(0, latest.id.clone());
        }
        for (i, v) in versions.into_iter().enumerate() {
            quick = quick.child(
                Button::new(("hub-mc", i), v.clone())
                    .disabled(disabled)
                    .on_click(cx.listener(move |home, _, _, cx| {
                        home.hub.mc.update(cx, |f, cx| f.set_text(v.clone(), cx));
                        home.hub.clear();
                        cx.notify();
                    })),
            );
        }
        filters = filters.child(quick);
        if h.packs {
            let mut sources = div().flex().gap_2();
            for (i, src) in [Source::Modrinth, Source::CurseForge]
                .into_iter()
                .enumerate()
            {
                sources = sources.child(
                    Button::new(("source", i), src.label())
                        .when(h.source == src, |b| b.primary())
                        .disabled(disabled)
                        .on_click(cx.listener(move |home, _, _, cx| {
                            home.hub.source = src;
                            home.hub.clear();
                            cx.notify();
                        })),
                );
            }
            filters = filters
                .child(kicker("整合包来源"))
                .child(sources)
                .child(Input::new(&h.query));
        }
        filters = filters.child(
            Button::new(
                "hub-search",
                if h.loading {
                    "正在查询"
                } else if h.packs {
                    "搜索整合包"
                } else {
                    "查询加载器版本"
                },
            )
            .primary()
            .disabled(disabled)
            .on_click(cx.listener(|home, _, _, cx| home.lookup(0, None, false, cx))),
        );
        let mut layout = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(heading("下载中心"))
                    .child(
                        Button::new("hub-import", "导入本地整合包")
                            .disabled(disabled)
                            .on_click(cx.listener(|home, _, _, cx| home.import_pack(cx))),
                    ),
            )
            .child(tabs)
            .child(filters);
        let mut rows = div().flex().flex_col().gap_2();
        if h.packs && h.project.is_none() {
            for (i, hit) in h.hits.iter().enumerate() {
                let hit = hit.clone();
                rows = rows.child(
                    panel(&hit.title)
                        .child(div().text_sm().child(hit.description.clone()))
                        .child(
                            Button::new(("pack-project", i), "选择整合包版本")
                                .disabled(disabled)
                                .on_click(cx.listener(move |home, _, _, cx| {
                                    home.lookup(home.hub.offset, Some(hit.clone()), false, cx)
                                })),
                        ),
                );
            }
            if h.offset > 0 || h.hits.len() == 10 {
                rows = rows.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new("packs-prev", "上一页")
                                .disabled(disabled || h.offset == 0)
                                .on_click(cx.listener(|home, _, _, cx| {
                                    home.lookup(home.hub.offset.saturating_sub(10), None, true, cx)
                                })),
                        )
                        .child(
                            Button::new("packs-next", "下一页")
                                .disabled(disabled || h.hits.len() < 10)
                                .on_click(cx.listener(|home, _, _, cx| {
                                    home.lookup(home.hub.offset + 10, None, true, cx)
                                })),
                        ),
                );
            }
        } else {
            let total = if h.packs {
                h.releases.len()
            } else {
                h.builds.len()
            };
            if let Some(title) = &h.project {
                rows = rows.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(kicker(title))
                        .child(
                            Button::new("pack-back", "返回搜索结果")
                                .disabled(disabled)
                                .on_click(cx.listener(|home, _, _, cx| {
                                    home.hub.project = None;
                                    home.hub.page = 0;
                                    cx.notify();
                                })),
                        ),
                );
            }
            for i in (h.page * 10..total).take(10) {
                let name = if h.packs {
                    h.releases[i].name.clone()
                } else {
                    format!(
                        "Minecraft {}    /    {} {}",
                        h.result_mc,
                        h.loader.label(),
                        h.builds[i]
                    )
                };
                rows = rows.child(
                    div()
                        .bg(rgb(PANEL))
                        .rounded_md()
                        .p_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(name))
                        .child(
                            Button::new(
                                ("install-result", i),
                                if h.packs {
                                    "预览整合包"
                                } else {
                                    "安装到新实例"
                                },
                            )
                            .disabled(disabled)
                            .on_click(cx.listener(
                                move |home, _, _, cx| {
                                    if home.hub.packs {
                                        home.install_pack(
                                            Pack::Online(home.hub.releases[i].clone()),
                                            cx,
                                        );
                                    } else {
                                        let result =
                                            home.install_build(home.hub.builds[i].clone(), cx);
                                        home.message(result, cx);
                                    }
                                },
                            )),
                        ),
                );
            }
            if total > 10 {
                rows = rows.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("files-prev", "上一页")
                                .disabled(disabled || h.page == 0)
                                .on_click(cx.listener(|home, _, _, cx| {
                                    home.hub.page = home.hub.page.saturating_sub(1);
                                    cx.notify();
                                })),
                        )
                        .child(kicker(&format!("{} / {}", h.page + 1, total.div_ceil(10))))
                        .child(
                            Button::new("files-next", "下一页")
                                .disabled(disabled || (h.page + 1) * 10 >= total)
                                .on_click(cx.listener(|home, _, _, cx| {
                                    home.hub.page += 1;
                                    cx.notify();
                                })),
                        ),
                );
            }
        }
        layout = layout.child(rows);
        layout
    }
}
