use super::*;
use crate::folders::{self, Found};
pub(super) struct Manager {
    folders: Vec<PathBuf>,
    folder: Option<PathBuf>,
    found: Vec<Found>,
    query: Entity<TextInput>,
    _updates: Subscription,
    hidden: Vec<Loader>,
    scanning: bool,
    error: Option<String>,
}
impl Manager {
    pub(super) fn new(root: &std::path::Path, window: &mut Window, cx: &mut Context<Home>) -> Self {
        let query = cx.new(|cx| {
            TextInput::new(window, cx)
                .label("搜索实例")
                .placeholder("搜索名称、Minecraft 版本或加载器")
                .max_len(160)
        });
        let updates = cx.observe(&query, |_, _, cx| cx.notify());
        let (folders, error) = match folders::registered(root) {
            Ok(v) => (v, None),
            Err(e) => (vec![], Some(format!("{e:#}"))),
        };
        Self {
            folders,
            folder: None,
            found: vec![],
            query,
            _updates: updates,
            hidden: vec![],
            scanning: false,
            error,
        }
    }
}
impl Home {
    fn select_folder(&mut self, selected: Option<PathBuf>, register: bool, cx: &mut Context<Self>) {
        if self.manager.scanning {
            return;
        }
        self.manager.found.clear();
        self.manager.folder = selected.clone();
        self.manager.error = None;
        let Some(folder) = selected else {
            cx.notify();
            return;
        };
        let root = self.root.clone();
        self.manager.scanning = true;
        let task = cx.background_executor().spawn(async move {
            let folder = if register {
                folders::register(&root, &folder)?
            } else {
                folder
            };
            let found = folders::scan(&folder)?;
            Ok::<_, anyhow::Error>((folder, found))
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |home, cx| {
                let m = &mut home.manager;
                m.scanning = false;
                match result {
                    Ok((p, found)) => {
                        if !m.folders.contains(&p) {
                            m.folders.push(p.clone());
                        }
                        m.folder = Some(p);
                        m.found = found;
                    }
                    Err(e) => m.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn pick_folder(&mut self, cx: &mut Context<Self>) {
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择已有的 .minecraft 目录".into()),
        });
        cx.spawn(async move |view, cx| {
            let result = picker.await;
            let _ = view.update(cx, |home, cx| match result {
                Ok(Ok(Some(v))) => {
                    if let Some(folder) = v.into_iter().next() {
                        home.select_folder(Some(folder), true, cx);
                    }
                }
                Ok(Err(e)) => home.message(Err(e), cx),
                _ => {}
            });
        })
        .detach();
    }
    fn bring_instance(&mut self, found: Found, cx: &mut Context<Self>) {
        if self.copying || self.cancel.is_some() {
            return;
        }
        let root = self.root.clone();
        self.copying = true;
        self.failed = false;
        self.status = "正在复制实例的游戏文件".into();
        let task = cx
            .background_executor()
            .spawn(async move { folders::import(&root, &found) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |home, cx| {
                home.copying = false;
                match result {
                    Ok(item) => {
                        home.instances.push(item);
                        home.show_instance(home.instances.len() - 1, cx);
                        home.status = "已复制到实例库".into();
                    }
                    Err(e) => home.message(Err(e), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn show_instance(&mut self, index: usize, cx: &mut Context<Self>) {
        self.controls.reset_selection();
        self.selected = index;
        self.page = Page::Detail;
        (self.failed, self.status) = self
            .outcomes
            .get(&self.instances[index].id)
            .cloned()
            .unwrap_or_else(|| (false, "就绪".into()));
        cx.notify();
    }
    pub(super) fn manager_page(&self, cx: &Context<Self>) -> impl IntoElement {
        let m = &self.manager;
        let busy = m.scanning || self.copying;
        let mut roots = div().flex().flex_wrap().gap_2().child(
            Button::new("all-instances", "我的实例")
                .when(m.folder.is_none(), |b| b.primary())
                .disabled(busy)
                .on_click(cx.listener(|home, _, _, cx| home.select_folder(None, false, cx))),
        );
        for (i, p) in m.folders.iter().enumerate() {
            let path = p.clone();
            let label = path.display().to_string();
            roots = roots.child(
                Button::new(
                    ("registered", i),
                    label.trim_start_matches(r"\\?\").to_owned(),
                )
                .when(m.folder.as_ref() == Some(p), |b| b.primary())
                .disabled(busy)
                .on_click(cx.listener(move |home, _, _, cx| {
                    home.select_folder(Some(path.clone()), false, cx)
                })),
            );
        }
        let mut page = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(heading("实例管理"))
            .child(
                panel("游戏目录").child(roots).child(
                    div()
                        .flex()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            Button::new("add-minecraft", "添加已有目录")
                                .disabled(busy)
                                .on_click(cx.listener(|home, _, _, cx| home.pick_folder(cx))),
                        )
                        .child(
                            Button::new("manager-import-pack", "导入整合包")
                                .disabled(busy)
                                .on_click(cx.listener(|home, _, _, cx| home.import_pack(cx))),
                        ),
                ),
            )
            .child(Input::new(&m.query));
        if let Some(error) = &m.error {
            page = page.child(div().text_color(rgb(0xffb5c5)).child(error.clone()));
        }
        let query = m.query.read(cx).text().trim().to_lowercase();
        let matches = |name: &str, mc: &str, loader: Loader, build: Option<&str>| {
            format!("{name} {mc} {} {}", loader.label(), build.unwrap_or(""))
                .to_lowercase()
                .contains(&query)
        };
        let mut displayed = 0;
        for (group, loader) in [
            Loader::NeoForge,
            Loader::Forge,
            Loader::Fabric,
            Loader::Quilt,
            Loader::Vanilla,
        ]
        .into_iter()
        .enumerate()
        {
            let mut rows = div().flex().flex_col().gap_2();
            let mut count = 0usize;
            let mut row = |name: &str,
                           version: &str,
                           build: Option<&str>,
                           info: &str,
                           index: Option<usize>,
                           found: Option<Found>| {
                let label = format!(
                    "Minecraft {version}    ·    {} {}    ·    {info}",
                    loader.label(),
                    build.unwrap_or("")
                );
                let key = count;
                count += 1;
                div()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(PANEL))
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(name.to_owned()),
                            )
                            .child(kicker(&label)),
                    )
                    .child(
                        Button::new(
                            (loader.label(), key),
                            if index.is_some() {
                                "打开实例"
                            } else {
                                "复制到实例库"
                            },
                        )
                        .disabled(busy)
                        .on_click(cx.listener(move |home, _, _, cx| {
                            if let Some(i) = index {
                                home.show_instance(i, cx);
                            } else if let Some(f) = found.clone() {
                                home.bring_instance(f, cx);
                            }
                        })),
                    )
            };
            if m.folder.is_none() {
                for (i, item) in self.instances.iter().enumerate().filter(|(_, i)| {
                    i.loader == loader
                        && matches(&i.name, &i.version, loader, i.loader_version.as_deref())
                }) {
                    let info = self
                        .running
                        .get(&item.id)
                        .map(|pid| format!("运行中 · PID {pid}"))
                        .unwrap_or_else(|| "独立实例".into());
                    rows = rows.child(row(
                        &item.name,
                        &item.version,
                        item.loader_version.as_deref(),
                        &info,
                        Some(i),
                        None,
                    ));
                }
            } else {
                for f in m.found.iter().filter(|f| {
                    f.loader == loader
                        && matches(&f.origin.profile, &f.version, loader, f.build.as_deref())
                }) {
                    let index = self
                        .instances
                        .iter()
                        .position(|i| i.origin.as_ref() == Some(&f.origin));
                    rows = rows.child(row(
                        &f.origin.profile,
                        &f.version,
                        f.build.as_deref(),
                        if f.isolated {
                            "版本目录"
                        } else {
                            "共用游戏目录"
                        },
                        index,
                        Some(f.clone()),
                    ));
                }
            }
            if count == 0 {
                continue;
            }
            displayed += count;
            let closed = m.hidden.contains(&loader);
            page = page.child(
                Button::new(
                    ("manage-group", group),
                    format!(
                        "{}   {} ({count})",
                        if closed { "▸" } else { "▾" },
                        loader.label()
                    ),
                )
                .on_click(cx.listener(move |home, _, _, cx| {
                    if home.manager.hidden.contains(&loader) {
                        home.manager.hidden.retain(|v| *v != loader);
                    } else {
                        home.manager.hidden.push(loader);
                    }
                    cx.notify();
                })),
            );
            if !closed {
                page = page.child(rows);
            }
        }
        if displayed == 0 {
            page = page.child(kicker(if m.scanning {
                "正在读取游戏目录…"
            } else {
                "没有找到匹配的实例"
            }));
        }
        page.child(self.removed_panel(cx))
    }
}
