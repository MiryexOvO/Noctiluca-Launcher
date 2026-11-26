use super::*;

#[derive(Default)]
pub(super) struct Wizard {
    filter: String,
    snapshots: bool,
    game_page: usize,
    game: Option<String>,
    loader: Option<Loader>,
    build_page: usize,
    builds: Vec<String>,
    build: Option<String>,
    request: u64,
    loading: bool,
    error: Option<String>,
}

impl Wizard {
    fn loader(&self) -> Loader {
        self.loader.unwrap_or(Loader::Vanilla)
    }

    fn matches(&self, v: &game::Entry) -> bool {
        (self.snapshots || v.kind == "release") && v.id.to_ascii_lowercase().contains(&self.filter)
    }
}

impl Home {
    pub(super) fn filter_versions(&mut self, filter: String, cx: &mut Context<Self>) {
        if self.wizard.filter != filter {
            self.wizard.filter = filter;
            self.wizard.game_page = 0;
            self.sync_game_choice(cx);
        }
        cx.notify();
    }

    pub(super) fn sync_game_choice(&mut self, cx: &mut Context<Self>) {
        let game = self
            .versions
            .iter()
            .filter(|v| self.wizard.matches(v))
            .find(|v| v.id.eq_ignore_ascii_case(&self.wizard.filter))
            .or_else(|| {
                self.versions
                    .iter()
                    .find(|v| self.wizard.game.as_ref() == Some(&v.id) && self.wizard.matches(v))
            })
            .or_else(|| self.versions.iter().find(|v| self.wizard.matches(v)))
            .map(|v| v.id.clone());
        if game != self.wizard.game {
            self.wizard.game = game;
            self.fetch_builds(cx);
        }
    }

    fn fetch_builds(&mut self, cx: &mut Context<Self>) {
        let w = &mut self.wizard;
        w.request += 1;
        w.build = None;
        w.builds.clear();
        w.build_page = 0;
        w.error = None;
        w.loading = false;
        let request = w.request;
        let loader = w.loader();
        let Some(mc) = w.game.clone().filter(|_| loader != Loader::Vanilla) else {
            cx.notify();
            return;
        };
        w.loading = true;
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(200))
                .await;
            if view
                .read_with(cx, |home, _| home.wizard.request != request)
                .unwrap_or(true)
            {
                return;
            }
            let result = cx
                .background_executor()
                .spawn(async move { crate::market::builds(loader, &mc) })
                .await;
            let _ = view.update(cx, |home, cx| {
                let w = &mut home.wizard;
                if w.request != request {
                    return;
                }
                w.loading = false;
                match result {
                    Ok(builds) => {
                        w.build = builds.first().cloned();
                        w.builds = builds;
                    }
                    Err(e) => w.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn create_and_install(&mut self, cx: &mut Context<Self>) -> Result<()> {
        ensure!(
            self.cancel.is_none() && !self.logging_in && !self.copying,
            "请等待当前操作完成"
        );
        let w = &self.wizard;
        let version = w
            .game
            .as_ref()
            .filter(|id| self.versions.iter().any(|v| &v.id == *id))
            .ok_or_else(|| anyhow::anyhow!("请选择游戏版本"))?;
        let loader = w.loader();
        ensure!(
            loader == Loader::Vanilla || w.build.as_ref().is_some_and(|id| w.builds.contains(id)),
            "请选择兼容的加载器版本"
        );
        let entered = self.name.read(cx).text().trim();
        let name = if entered.is_empty() {
            format!("{} {version}", loader.label())
        } else {
            entered.to_owned()
        };
        let mut item = Instance::new(name, version.clone(), loader)?;
        item.loader_version = w.build.clone();
        library::save(&self.root, &item)?;
        let previous = self.selected;
        self.instances.push(item);
        self.selected = self.instances.len() - 1;
        self.page = Page::Detail;
        self.play(true, cx);
        if !crate::preferences::current().select_new {
            self.selected = previous;
        }
        self.name.update(cx, |f, cx| f.set_text("", cx));
        Ok(())
    }

    fn version_row(&self, values: &[&str], size: usize, builds: bool, cx: &Context<Self>) -> Div {
        let w = &self.wizard;
        let (key, page, selected) = if builds {
            ("build", w.build_page, &w.build)
        } else {
            ("mc", w.game_page, &w.game)
        };
        let last_page = values.len().saturating_sub(1) / size;
        let page = page.min(last_page);
        let mut choices = div()
            .flex()
            .flex_1()
            .min_w_0()
            .justify_center()
            .flex_wrap()
            .gap_1();
        for (i, &value) in values.iter().skip(page * size).take(size).enumerate() {
            let value = value.to_owned();
            choices = choices.child(
                Button::new((key, i), value.clone())
                    .when(selected.as_ref() == Some(&value), |b| b.primary())
                    .on_click(cx.listener(move |home, _, _, cx| {
                        if builds {
                            home.wizard.build = Some(value.clone());
                        } else if home.wizard.game.as_ref() != Some(&value) {
                            home.wizard.game = Some(value.clone());
                            home.fetch_builds(cx);
                        }
                        cx.notify();
                    })),
            );
        }
        let arrow = |next: bool| {
            let key = match (builds, next) {
                (false, false) => "mc-prev",
                (false, true) => "mc-next",
                (true, false) => "build-prev",
                (true, true) => "build-next",
            };
            IconButton::new(
                key,
                if next {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronLeft
                },
            )
            .tooltip(if next { "下一页" } else { "上一页" })
            .disabled(if next { page == last_page } else { page == 0 })
            .on_click(cx.listener(move |home, _, _, cx| {
                let n = if next {
                    (page + 1).min(last_page)
                } else {
                    page.saturating_sub(1)
                };
                if builds {
                    home.wizard.build_page = n;
                } else {
                    home.wizard.game_page = n;
                }
                cx.notify();
            }))
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(arrow(false))
            .child(choices)
            .child(arrow(true))
    }

    pub(super) fn form(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let w = &self.wizard;
        let values: Vec<_> = self
            .versions
            .iter()
            .filter(|v| w.matches(v))
            .map(|v| v.id.as_str())
            .collect();
        let page_size = if window.viewport_size().width < px(920.) {
            3
        } else {
            5
        };
        let mut games = panel(cx, "游戏版本").child(Input::new(&self.query)).child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new("stable", "正式版")
                        .when(!w.snapshots, |b| b.primary())
                        .on_click(cx.listener(|home, _, _, cx| {
                            home.wizard.snapshots = false;
                            home.wizard.game_page = 0;
                            home.sync_game_choice(cx);
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("with-snapshots", "含快照")
                        .when(w.snapshots, |b| b.primary())
                        .on_click(cx.listener(|home, _, _, cx| {
                            home.wizard.snapshots = true;
                            home.wizard.game_page = 0;
                            home.sync_game_choice(cx);
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("refresh", "刷新版本")
                        .disabled(self.fetching)
                        .on_click(cx.listener(|home, _, _, cx| {
                            home.fetch_versions(true, cx);
                            cx.notify();
                        })),
                ),
        );
        games = if values.is_empty() {
            games.child(kicker(
                cx,
                if self.fetching {
                    "正在获取官方版本…"
                } else {
                    "没有匹配的游戏版本"
                },
            ))
        } else {
            games.child(self.version_row(&values, page_size, false, cx))
        };
        let mut loaders = div().flex().gap_2().flex_wrap();
        for (i, loader) in [
            Loader::Vanilla,
            Loader::Fabric,
            Loader::Forge,
            Loader::NeoForge,
            Loader::Quilt,
        ]
        .into_iter()
        .enumerate()
        {
            if loader == Loader::Quilt && crate::preferences::current().hide_quilt {
                continue;
            }
            loaders = loaders.child(
                Button::new(("loader", i), loader.label())
                    .when(w.loader() == loader, |b| b.primary())
                    .on_click(cx.listener(move |home, _, _, cx| {
                        if home.wizard.loader() != loader {
                            home.wizard.loader = Some(loader);
                            home.fetch_builds(cx);
                        }
                    })),
            );
        }
        let mut components = panel(cx, "加载器").child(loaders);
        if w.loader() != Loader::Vanilla {
            components = components.child(kicker(cx, "加载器版本"));
            if w.builds.is_empty() {
                let message = w.error.as_deref().unwrap_or(if w.loading {
                    "正在查询兼容构建…"
                } else {
                    "该游戏版本没有可用构建"
                });
                components = components.child(kicker(cx, message));
                if w.error.is_some() {
                    components = components.child(
                        Button::new("retry-builds", "重新查询")
                            .on_click(cx.listener(|home, _, _, cx| home.fetch_builds(cx))),
                    );
                }
            } else {
                let ids: Vec<_> = w.builds.iter().map(String::as_str).collect();
                components = components.child(self.version_row(&ids, page_size.min(3), true, cx));
            }
        }
        let summary = match (&w.game, &w.build) {
            (Some(v), Some(b)) => format!("Minecraft {v} · {} {b}", w.loader().label()),
            (Some(v), None) => format!("Minecraft {v} · {}", w.loader().label()),
            _ => String::new(),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(heading("新建实例"))
                    .child(
                        Button::new(
                            "import-pack",
                            if self.draft.is_some() {
                                "继续导入整合包"
                            } else {
                                "导入整合包"
                            },
                        )
                        .on_click(cx.listener(|home, _, _, cx| home.import_pack(cx))),
                    ),
            )
            .child(kicker(cx, "实例名称"))
            .child(Input::new(&self.name))
            .child(games)
            .child(components)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        Button::new("create", "创建并安装")
                            .primary()
                            .disabled(
                                w.game.is_none()
                                    || (w.loader() != Loader::Vanilla && w.build.is_none())
                                    || self.cancel.is_some()
                                    || self.logging_in
                                    || self.copying,
                            )
                            .on_click(cx.listener(|home, _, _, cx| {
                                let r = home.create_and_install(cx);
                                home.message(r, cx);
                            })),
                    )
                    .child(kicker(cx, &summary)),
            )
    }
}
