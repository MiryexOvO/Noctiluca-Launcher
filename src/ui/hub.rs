use super::*;
use crate::download::Downloads;
use crate::market::{self, Hit, Kind, Release, Source};
use anyhow::Context as _;

pub(super) enum Pack {
    Local(PathBuf),
    Dropped(PathBuf),
    UpdateLocal(PathBuf, Instance),
    UpdateOnline(Release, Instance),
    Retry(PathBuf, Option<crate::content::Upstream>, Option<Instance>),
    Online(Release),
}

pub(super) struct Hub {
    kind: Kind,
    loader: Option<Loader>,
    pub(super) source: Source,
    mc: Entity<TextInput>,
    query: Entity<TextInput>,
    _search_subscriptions: [Subscription; 2],
    hits: Vec<Hit>,
    releases: Vec<Release>,
    project: Option<Hit>,
    target: Option<Instance>,
    result_mc: String,
    result_query: String,
    offset: usize,
    page: usize,
    request: u64,
    pub(super) searched: bool,
    pub(super) loading: bool,
    error: Option<String>,
}
impl Hub {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Home>) -> Self {
        let mc = cx.new(|cx| {
            TextInput::new(window, cx)
                .label("游戏版本筛选")
                .placeholder("所有游戏版本，例如 1.20.1")
                .max_len(80)
        });
        let query = cx.new(|cx| {
            TextInput::new(window, cx)
                .label("搜索内容")
                .placeholder("搜索项目名称")
                .max_len(100)
        });
        let subscriptions = [&query, &mc].map(|field| {
            cx.subscribe(
                field,
                |home, _, event: &ely_gpui_component::forms::InputEvent, cx| {
                    if *event == ely_gpui_component::forms::InputEvent::Submit
                        && home.page == Page::Hub
                        && home.hub.project.is_none()
                    {
                        home.discover(0, false, cx);
                    }
                },
            )
        });
        Self {
            kind: Kind::Mod,
            loader: None,
            source: if crate::preferences::current().community == 1 {
                Source::CurseForge
            } else {
                Source::Modrinth
            },
            mc,
            query,
            _search_subscriptions: subscriptions,
            hits: vec![],
            releases: vec![],
            project: None,
            target: None,
            result_mc: String::new(),
            result_query: String::new(),
            offset: 0,
            page: 0,
            request: 0,
            searched: false,
            loading: false,
            error: None,
        }
    }
    pub(super) fn is_pack(&self) -> bool {
        self.kind == Kind::Pack
    }
    fn back(&mut self) {
        self.request += 1;
        self.loading = false;
        self.releases.clear();
        self.page = 0;
        self.target = None;
        self.project = None;
        self.error = None;
    }
    pub(super) fn clear(&mut self) {
        self.back();
        self.hits.clear();
        self.searched = false;
        self.offset = 0;
    }
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
            let dropped = matches!(&pack, Pack::Dropped(_));
            let result = (|| -> Result<_> {
                let mut retry_origin = None;
                let (file, target) = match pack {
                    Pack::Retry(path, origin, target) => {
                        retry_origin = origin;
                        (Ok(path), target)
                    }
                    Pack::Local(path) | Pack::Dropped(path) => (Ok(path), None),
                    Pack::UpdateLocal(path, item) => (Ok(path), Some(item)),
                    Pack::Online(file) => (Err(file), None),
                    Pack::UpdateOnline(file, item) => (Err(file), Some(item)),
                };
                let (path, upstream) = match file {
                    Ok(path) => (path, None),
                    Err(file) => (
                        file.archive(&root, &Downloads::new(&cancel, &report)?)?,
                        Some(crate::content::Upstream::from(&file)),
                    ),
                };
                let mut preview = crate::packs::preview(&root, &path)?;
                preview.upstream = upstream.or(retry_origin);
                preview.target = target;
                anyhow::ensure!(!cancel.load(Ordering::Relaxed), "已取消预览");
                Ok((path, Box::new(preview)))
            })();
            let _ = tx.try_send(Update::PackPreview(result, dropped));
        });
        self.listen(rx, cx);
    }

    pub(super) fn discover(&mut self, offset: usize, paging: bool, cx: &mut Context<Self>) {
        let h = &mut self.hub;
        if !paging {
            h.result_mc = h.mc.read(cx).text().trim().to_owned();
            h.result_query = h.query.read(cx).text().trim().to_owned();
        }
        h.clear();
        h.offset = offset;
        h.loading = true;
        h.searched = true;
        let (source, kind, loader, mc, q, request) = (
            h.source,
            h.kind,
            h.loader,
            h.result_mc.clone(),
            h.result_query.clone(),
            h.request,
        );
        let task = cx
            .background_executor()
            .spawn(async move { market::search(source, kind, loader, &mc, &q, offset) });
        cx.spawn(async move |view, cx| {
            let r = task.await;
            let _ = view.update(cx, |home, cx| {
                let h = &mut home.hub;
                if h.request != request {
                    return;
                }
                h.loading = false;
                match r {
                    Ok(v) => h.hits = v,
                    Err(e) => h.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_project(&mut self, hit: Hit, cx: &mut Context<Self>) {
        self.hub.back();
        self.hub.project = Some(hit);
        if self.hub.is_pack() {
            self.discover_files(None, cx);
        } else {
            cx.notify();
        }
    }

    fn discover_files(&mut self, target: Option<Instance>, cx: &mut Context<Self>) {
        let h = &mut self.hub;
        let Some(hit) = &h.project else {
            return;
        };
        let (loader, mc) = match &target {
            Some(i) => (Some(i.loader), i.version.clone()),
            None if h.is_pack() => (h.loader, h.result_mc.clone()),
            _ => return,
        };
        h.target = target;
        h.page = 0;
        h.releases.clear();
        h.loading = true;
        h.error = None;
        h.request += 1;
        let (source, kind, project, request) = (h.source, h.kind, hit.id.clone(), h.request);
        let task = cx
            .background_executor()
            .spawn(async move { market::files(source, kind, loader, &mc, &project) });
        cx.spawn(async move |view, cx| {
            let r = task.await;
            let _ = view.update(cx, |home, cx| {
                let h = &mut home.hub;
                if h.request != request {
                    return;
                }
                h.loading = false;
                match r {
                    Ok(v) => h.releases = v,
                    Err(e) => h.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn install_discovery_file(&mut self, file: Release, cx: &mut Context<Self>) {
        if self.cancel.is_some() || self.copying || self.logging_in {
            return;
        }
        if self.hub.is_pack() {
            self.install_pack(Pack::Online(file), cx);
            return;
        }
        let Some(item) = self.hub.target.clone() else {
            return;
        };
        if self.running.contains_key(&item.id) {
            self.message(Err(anyhow::anyhow!("请退出目标实例后安装")), cx);
            return;
        }
        let kind = self.hub.kind;
        let root = self.root.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.failed = false;
        self.status = format!("正在安装到「{}」", item.name);
        let (tx, rx) = async_channel::unbounded();
        std::thread::spawn(move || {
            let r = market::install_file(&root, &item, kind, &file, &cancel, |s| {
                let _ = tx.try_send(Update::Status(s));
            })
            .with_context(|| format!("安装到「{}」失败", item.name));
            let _ = tx.try_send(Update::Finished(
                r,
                format!("{}已安装到「{}」", kind.label(), item.name),
            ));
        });
        self.listen(rx, cx);
    }

    fn discovery_project(&self, hit: &Hit, cx: &Context<Self>) -> Div {
        let h = &self.hub;
        let busy = self.cancel.is_some() || self.logging_in || self.copying;
        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(heading(&hit.title))
                    .child(
                        Button::new("discover-back", "返回搜索结果").on_click(cx.listener(
                            |home, _, _, cx| {
                                home.hub.back();
                                cx.notify();
                            },
                        )),
                    ),
            )
            .child(kicker(&hit.description));
        if !h.is_pack() && h.target.is_none() {
            let mut targets = panel("选择安装到哪个实例");
            if self.instances.is_empty() {
                targets = targets.child(kicker("实例库为空，请先创建实例。")).child(
                    Button::new("first-instance", "新建实例").on_click(cx.listener(
                        |home, _, _, cx| {
                            home.page = Page::Create;
                            home.fetch_versions(false, cx);
                            cx.notify();
                        },
                    )),
                );
            }
            for (i, item) in self.instances.iter().enumerate() {
                let id = item.id.clone();
                let running = self.running.contains_key(&id);
                let vanilla = h.kind == Kind::Mod && item.loader == Loader::Vanilla;
                targets = targets.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(item.name.clone()))
                        .child(kicker(&format!(
                            "{} · {}",
                            item.version,
                            item.loader.label()
                        )))
                        .child(
                            Button::new(
                                ("target", i),
                                if running {
                                    "运行中"
                                } else if vanilla {
                                    "需要加载器"
                                } else {
                                    "选择实例"
                                },
                            )
                            .disabled(busy || running || vanilla)
                            .on_click(cx.listener(
                                move |home, _, _, cx| {
                                    if let Some(item) =
                                        home.instances.iter().find(|v| v.id == id).cloned()
                                    {
                                        home.discover_files(Some(item), cx);
                                    }
                                },
                            )),
                        ),
                );
            }
            return body.child(targets);
        }
        if let Some(item) = &h.target {
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(format!(
                        "安装到「{}」 · Minecraft {} · {}",
                        item.name,
                        item.version,
                        item.loader.label()
                    )))
                    .child(
                        Button::new("change-target", "更换实例").on_click(cx.listener(
                            |home, _, _, cx| {
                                home.hub.request += 1;
                                home.hub.loading = false;
                                home.hub.target = None;
                                home.hub.releases.clear();
                                home.hub.error = None;
                                cx.notify();
                            },
                        )),
                    ),
            );
        }
        let mut files = panel(if h.is_pack() {
            "整合包版本"
        } else {
            "兼容版本"
        });
        if h.loading {
            return body.child(files.child(kicker("正在查询发布文件…")));
        }
        if let Some(error) = &h.error {
            return body.child(files.child(kicker(error)).child(
                Button::new("retry-files", "重新查询").on_click(
                    cx.listener(|home, _, _, cx| home.discover_files(home.hub.target.clone(), cx)),
                ),
            ));
        }
        if h.releases.is_empty() {
            return body.child(files.child(kicker("没有符合游戏版本和加载器条件的发布文件")));
        }
        for (i, file) in h.releases.iter().enumerate().skip(h.page * 10).take(10) {
            let file = file.clone();
            files = files.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(file.name.clone()))
                    .child(
                        Button::new(
                            ("discovery-install", i),
                            if h.is_pack() {
                                "预览整合包"
                            } else {
                                "安装此版本"
                            },
                        )
                        .disabled(busy)
                        .on_click(cx.listener(move |home, _, _, cx| {
                            home.install_discovery_file(file.clone(), cx)
                        })),
                    ),
            );
        }
        if h.releases.len() > 10 {
            files = files.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("files-prev", "上一页")
                            .disabled(h.page == 0)
                            .on_click(cx.listener(|home, _, _, cx| {
                                home.hub.page = home.hub.page.saturating_sub(1);
                                cx.notify();
                            })),
                    )
                    .child(kicker(&format!(
                        "{} / {}",
                        h.page + 1,
                        h.releases.len().div_ceil(10)
                    )))
                    .child(
                        Button::new("files-next", "下一页")
                            .disabled((h.page + 1) * 10 >= h.releases.len())
                            .on_click(cx.listener(|home, _, _, cx| {
                                home.hub.page += 1;
                                cx.notify();
                            })),
                    ),
            );
        }
        body.child(files)
    }

    pub(super) fn hub_page(&self, cx: &Context<Self>) -> Div {
        let h = &self.hub;
        if let Some(project) = &h.project {
            return self.discovery_project(project, cx);
        }
        let mut tabs = div().flex().flex_wrap().gap_2();
        for (i, kind) in [Kind::Mod, Kind::Pack, Kind::Resource, Kind::Shader]
            .into_iter()
            .enumerate()
        {
            tabs = tabs.child(
                Button::new(("discover-kind", i), kind.label())
                    .when(kind == h.kind, |b| b.primary())
                    .on_click(cx.listener(move |home, _, _, cx| {
                        home.hub.kind = kind;
                        home.discover(0, false, cx);
                    })),
            );
        }
        let mut sources = div().flex().gap_1();
        for (i, source) in [Source::Modrinth, Source::CurseForge]
            .into_iter()
            .enumerate()
        {
            sources = sources.child(
                Button::new(("discover-source", i), source.label())
                    .when(source == h.source, |b| b.primary())
                    .on_click(cx.listener(move |home, _, _, cx| {
                        home.hub.source = source;
                        home.discover(0, false, cx);
                    })),
            );
        }
        let mut filters = panel("搜索与筛选")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&h.query)))
                    .child(
                        Button::new("search-content", "搜索")
                            .primary()
                            .on_click(cx.listener(|home, _, _, cx| home.discover(0, false, cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&h.mc)))
                    .child(sources),
            );
        if h.kind.uses_loader() {
            let mut loaders = div().flex().flex_wrap().gap_1();
            for (i, loader) in [
                None,
                Some(Loader::Fabric),
                Some(Loader::Forge),
                Some(Loader::NeoForge),
                Some(Loader::Quilt),
            ]
            .into_iter()
            .enumerate()
            {
                if loader == Some(Loader::Quilt) && crate::preferences::current().hide_quilt {
                    continue;
                }
                loaders = loaders.child(
                    Button::new(
                        ("discover-loader", i),
                        loader.map_or("全部加载器", Loader::label),
                    )
                    .when(loader == h.loader, |b| b.primary())
                    .on_click(cx.listener(move |home, _, _, cx| {
                        home.hub.loader = loader;
                        home.discover(0, false, cx);
                    })),
                );
            }
            filters = filters.child(loaders);
        }
        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(heading("发现内容"))
            .child(tabs)
            .child(filters);
        if h.loading {
            return body.child(kicker("正在搜索…"));
        }
        if let Some(error) = &h.error {
            return body.child(kicker(error));
        }
        if h.hits.is_empty() && h.searched {
            body = body.child(kicker("没有匹配的项目"));
        }
        for (i, hit) in h.hits.iter().enumerate() {
            let hit = hit.clone();
            body = body.child(
                panel(&hit.title).child(kicker(&hit.description)).child(
                    Button::new(
                        ("discover-project", i),
                        if h.is_pack() {
                            "选择整合包版本"
                        } else {
                            "下载"
                        },
                    )
                    .on_click(
                        cx.listener(move |home, _, _, cx| home.open_project(hit.clone(), cx)),
                    ),
                ),
            );
        }
        if h.offset > 0 || h.hits.len() == 10 {
            body = body.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("projects-prev", "上一页")
                            .disabled(h.offset == 0)
                            .on_click(cx.listener(|home, _, _, cx| {
                                home.discover(home.hub.offset.saturating_sub(10), true, cx)
                            })),
                    )
                    .child(
                        Button::new("projects-next", "下一页")
                            .disabled(h.hits.len() < 10)
                            .on_click(cx.listener(|home, _, _, cx| {
                                home.discover(home.hub.offset + 10, true, cx)
                            })),
                    ),
            );
        }
        body
    }
}
