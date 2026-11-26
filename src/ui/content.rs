use super::*;
use crate::{
    content as model,
    market::{self, Release},
};
use anyhow::Context as _;
use std::path::Path;

#[derive(Default)]
pub(super) struct Tools {
    pub(super) kind: &'static str,
    pub(super) label: &'static str,
    names: Vec<String>,
    offset: usize,
    build_page: usize,
    pub(super) builds: Vec<String>,
    loader: Option<Loader>,
    releases: Vec<Release>,
    upstream: Option<model::Upstream>,
    archive_hash: Option<String>,
    resource_hits: Vec<market::Hit>,
    groups: [bool; 4],
    rollbacks: Vec<PathBuf>,
    confirm: Option<PathBuf>,
    pub(super) curse: bool,
    pub(super) upgrades: Vec<mods::Upgrade>,
}
impl Home {
    fn import_resource(&mut self, cx: &mut Context<Self>) {
        let item = self.instances[self.selected].clone();
        let root = self.root.clone();
        let kind = self.content.kind;
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("选择要添加到实例的资源文件".into()),
        });
        cx.spawn(async move |view, cx| {
            let paths = match picker.await {
                Ok(Ok(Some(v))) => v,
                Ok(Err(e)) => {
                    let _ = view.update(cx, |h, cx| h.message(Err(e), cx));
                    return;
                }
                _ => return,
            };
            let _ = view.update(cx, |h, cx| {
                h.copying = true;
                cx.notify();
            });
            let task = cx
                .background_executor()
                .spawn(async move { model::add_files(&root, &item, kind, &paths) });
            let result = task.await;
            let _ = view.update(cx, |h, cx| {
                h.copying = false;
                if h.page == Page::Files {
                    h.browse_files(h.content.label, h.content.kind, cx);
                }
                if result.is_ok() {
                    h.failed = false;
                    h.status = "资源已添加".into();
                }
                h.message(result, cx);
            });
        })
        .detach();
    }
    pub(super) fn browse_files(
        &mut self,
        label: &'static str,
        kind: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.content.curse = crate::preferences::current().community == 1;
        self.content.label = label;
        self.content.kind = kind;
        self.content.offset = 0;
        self.content.resource_hits.clear();
        self.page = Page::Files;
        self.content.names.clear();
        if kind == "saves" {
            self.read_worlds(cx);
            return;
        }
        let result = (|| -> Result<_> {
            let dir = model::resource_dir(&self.root, &self.instances[self.selected], kind)?;
            let mut names = fs::read_dir(dir)?
                .map(|e| e.map(|v| v.file_name().to_string_lossy().into_owned()))
                .collect::<std::io::Result<Vec<_>>>()?;
            names.sort_by_key(|s| s.to_lowercase());
            Ok(names)
        })();
        match result {
            Ok(names) => {
                self.content.names = names;
                self.failed = false;
                self.status = "就绪".into();
            }
            Err(e) => self.message(Err(e), cx),
        }
        cx.notify();
    }
    pub(super) fn files_page(&self, cx: &Context<Self>) -> Div {
        let t = &self.content;
        let mut page = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(kicker(cx, &self.instances[self.selected].name))
            .child(heading(t.label));
        let mut actions = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(
                Button::new("files-back", "返回实例").on_click(cx.listener(|h, _, _, cx| {
                    h.page = Page::Detail;
                    cx.notify();
                })),
            )
            .child(
                Button::new("files-explore", "在资源管理器中打开").on_click(cx.listener(
                    |h, _, _, cx| {
                        let r =
                            model::resource_dir(&h.root, &h.instances[h.selected], h.content.kind)
                                .and_then(|p| model::explore(&p));
                        if r.is_ok() {
                            h.failed = false;
                            h.status = "已打开资源目录".into();
                        }
                        h.message(r, cx);
                    },
                )),
            )
            .child(Button::new("files-refresh", "刷新").on_click(
                cx.listener(|h, _, _, cx| h.browse_files(h.content.label, h.content.kind, cx)),
            ));
        if matches!(t.kind, "mods" | "shaderpacks" | "resourcepacks") {
            actions = actions.child(
                Button::new("add-local-resource", "从文件添加")
                    .disabled(self.copying || self.cancel.is_some())
                    .on_click(cx.listener(|h, _, _, cx| h.import_resource(cx))),
            );
        }
        if t.kind == "mods" {
            actions = actions.child(
                Button::new("files-mods", "管理模组")
                    .on_click(cx.listener(|h, _, _, cx| h.open_mods(cx))),
            );
        }
        if t.kind == "saves" {
            return self.world_rows(page, actions, cx);
        }
        page = page.child(actions);
        if matches!(t.kind, "resourcepacks" | "shaderpacks") {
            page = page.child(
                div()
                    .flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.mod_query)))
                    .child(
                        Button::new("resource-search", "搜索 Modrinth")
                            .disabled(self.searching || self.cancel.is_some())
                            .on_click(cx.listener(|h, _, _, cx| h.find_resources(cx))),
                    ),
            );
            for (i, hit) in t.resource_hits.iter().enumerate() {
                let hit = hit.clone();
                page = page.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(hit.title.clone())
                        .child(
                            Button::new(("resource-get", i), "安装")
                                .disabled(self.cancel.is_some())
                                .on_click(cx.listener(move |h, _, _, cx| {
                                    h.install_resource(hit.clone(), cx)
                                })),
                        ),
                );
            }
        }
        if t.names.is_empty() {
            return page.child(kicker(cx, "此目录还没有内容"));
        }
        for name in t.names.iter().skip(t.offset).take(40) {
            page = page.child(div().child(name.clone()));
        }
        if t.names.len() > 40 {
            page = page.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("files-prev", "上一页")
                            .disabled(t.offset == 0)
                            .on_click(cx.listener(|h, _, _, cx| {
                                h.content.offset = h.content.offset.saturating_sub(40);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("files-next", "下一页")
                            .disabled(t.offset + 40 >= t.names.len())
                            .on_click(cx.listener(|h, _, _, cx| {
                                h.content.offset += 40;
                                cx.notify();
                            })),
                    ),
            );
        }
        page
    }
    fn find_resources(&mut self, cx: &mut Context<Self>) {
        let kind = self.content.kind;
        let item = self.instances[self.selected].clone();
        let id = item.id.clone();
        let q = self.mod_query.read(cx).text().to_string();
        self.searching = true;
        self.status = "搜索 Modrinth".into();
        self.failed = false;
        let task = cx
            .background_executor()
            .spawn(async move { market::resources(kind, &item.version, &q) });
        cx.spawn(async move |view, cx| {
            let r = task.await;
            let _ = view.update(cx, |h, cx| {
                h.searching = false;
                if h.page == Page::Files
                    && h.instances[h.selected].id == id
                    && h.content.kind == kind
                {
                    match r {
                        Ok(hits) => {
                            h.status = format!("{} 个结果", hits.len());
                            h.content.resource_hits = hits;
                        }
                        Err(e) => h.message(Err(e), cx),
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn install_resource(&mut self, hit: market::Hit, cx: &mut Context<Self>) {
        let item = self.instances[self.selected].clone();
        let root = self.root.clone();
        let kind = self.content.kind;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.failed = false;
        self.status = "下载实例资源".into();
        let (tx, rx) = async_channel::unbounded();
        std::thread::spawn(move || {
            let r = market::install_resource(&root, &item, kind, &hit.id, &cancel, |s| {
                let _ = tx.try_send(Update::Status(s));
            });
            let _ = tx.try_send(Update::Finished(r, "资源已安装".into()));
        });
        self.listen(rx, cx);
    }
    pub(super) fn open_content(&mut self, cx: &mut Context<Self>) {
        let item = &self.instances[self.selected];
        self.content = Tools {
            groups: [true; 4],
            loader: Some(if item.loader == Loader::Vanilla {
                Loader::Fabric
            } else {
                item.loader
            }),
            ..Default::default()
        };
        self.page = Page::Content;
        match model::PackState::read(&item.path(&self.root)) {
            Ok(Some(state)) => {
                self.content.upstream = state.upstream;
                self.content.archive_hash = state.archive_sha1;
            }
            Ok(None) => {}
            Err(e) => self.message(Err(e), cx),
        }
        self.content.rollbacks = backups::list(&self.root, &self.instances[self.selected])
            .unwrap_or_default()
            .into_iter()
            .filter(|b| b.path.join("before-pack-update").is_file())
            .map(|b| b.path)
            .collect();
        cx.notify();
    }
    fn get_content_versions(&mut self, pack: bool, cx: &mut Context<Self>) {
        let item = self.instances[self.selected].clone();
        let id = item.id.clone();
        let loader = self.content.loader.unwrap_or(item.loader);
        let source = self.content.upstream.clone();
        let hash = self.content.archive_hash.clone();
        self.searching = true;
        self.status = "查询可用版本".into();
        self.failed = false;
        let task = cx.background_executor().spawn(async move {
            if pack {
                let u = match source {
                    Some(u) => u,
                    None => market::lookup_pack_hash(
                        &hash.context("此实例未关联在线项目，请选择本地新版包")?,
                    )?,
                };
                let mut versions = market::releases(u.source, item.loader, "", &u.project)?;
                if let Some(i) = versions.iter().position(|r| r.id == u.release) {
                    versions.truncate(i);
                }
                Ok((vec![], versions))
            } else {
                Ok((market::builds(loader, &item.version)?, vec![]))
            }
        });
        cx.spawn(async move |view, cx| {
            let result: Result<_> = task.await;
            let _ = view.update(cx, |h, cx| {
                h.searching = false;
                if h.page == Page::Content && h.instances[h.selected].id == id {
                    match result {
                        Ok((builds, releases)) => {
                            if pack {
                                h.content.releases = releases;
                                h.status = format!("{} 个其他发布版本", h.content.releases.len());
                            } else {
                                h.content.build_page = 0;
                                h.content.builds = builds;
                                h.status = format!("{} 个可用构建", h.content.builds.len());
                            }
                        }
                        Err(e) => h.message(Err(e), cx),
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn pick_update(&mut self, cx: &mut Context<Self>) {
        let item = self.instances[self.selected].clone();
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择此实例的新版整合包".into()),
        });
        cx.spawn(async move |view, cx| {
            let choice = picker.await;
            let _ = view.update(cx, |h, cx| match choice {
                Ok(Ok(Some(paths))) => {
                    if let Some(p) = paths.first() {
                        h.install_pack(hub::Pack::UpdateLocal(p.clone(), item), cx);
                    }
                }
                Ok(Err(e)) => h.message(Err(e), cx),
                _ => {}
            });
        })
        .detach();
    }
    fn save_archive(&mut self, report: bool, cx: &mut Context<Self>) {
        let item = self.instances[self.selected].clone();
        let name = if report {
            "Noctiluca-diagnostic.zip"
        } else {
            "modpack.mrpack"
        };
        let picker =
            cx.prompt_for_new_path(self.root.parent().unwrap_or(Path::new(".")), Some(name));
        let groups = self.content.groups;
        let root = self.root.clone();
        cx.spawn(async move |view, cx| {
            let path = match picker.await {
                Ok(Ok(Some(p))) => p,
                Ok(Err(e)) => {
                    let _ = view.update(cx, |h, cx| h.message(Err(e), cx));
                    return;
                }
                _ => return,
            };
            let _ = view.update(cx, |h, cx| {
                h.copying = true;
                h.status = "正在导出".into();
                cx.notify();
            });
            let task = cx.background_executor().spawn(async move {
                let mut path = path;
                if path.extension().is_none() {
                    path.set_extension(if report { "zip" } else { "mrpack" });
                }
                if report {
                    crate::logs::export(&root, &item, &path)?;
                } else {
                    model::export(&root, &item, &path, groups)?;
                }
                Ok(path)
            });
            let result: Result<PathBuf> = task.await;
            let _ = view.update(cx, |h, cx| {
                h.copying = false;
                match result {
                    Ok(p) => {
                        h.failed = false;
                        h.status = format!("已导出到 {}", p.display());
                    }
                    Err(e) => h.message(Err(e), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn export_report(&mut self, cx: &mut Context<Self>) {
        self.save_archive(true, cx);
    }
    fn change_component(&mut self, version: String, cx: &mut Context<Self>) {
        let item = self.instances[self.selected].clone();
        let root = self.root.clone();
        let loader = self.content.loader.unwrap_or(item.loader);
        self.failed = false;
        self.status = "准备加载器，校验成功后应用".into();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        let (tx, rx) = async_channel::unbounded();
        std::thread::spawn(move || {
            let result = model::component(&root, &item, loader, &version, &cancel, &|s| {
                let _ = tx.try_send(Update::Status(s));
            });
            let result = result.map(|item| {
                let _ = tx.try_send(Update::Changed(Box::new(item)));
            });
            let _ = tx.try_send(Update::Finished(result, "加载器已更新".into()));
        });
        self.listen(rx, cx);
        cx.notify();
    }

    fn rollback_pack(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.content.confirm.as_ref() != Some(&path) {
            self.content.confirm = Some(path);
            cx.notify();
            return;
        }
        let root = self.root.clone();
        let item = self.instances[self.selected].clone();
        self.copying = true;
        self.status = "正在回滚整合包".into();
        let task = cx
            .background_executor()
            .spawn(async move { model::rollback(&root, &item, &path) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |h, cx| {
                h.copying = false;
                match result {
                    Ok(item) => {
                        h.refresh_instance_icon(&item.id, cx);
                        if let Some(v) = h.instances.iter_mut().find(|v| v.id == item.id) {
                            *v = item;
                        }
                        h.open_content(cx);
                        h.failed = false;
                        h.status = "已回滚整合包".into();
                    }
                    Err(e) => h.message(Err(e), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn content_page(&self, cx: &Context<Self>) -> Div {
        let item = &self.instances[self.selected];
        let t = &self.content;
        let busy = self.searching || self.copying || self.cancel.is_some();
        let mut page = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(kicker(cx, &item.name))
            .child(heading("组件与整合包"));
        if item.external.is_none() {
            let mut components = panel(cx, "修改加载器").child(kicker(
                cx,
                &format!(
                    "Minecraft {} · 当前 {} {}",
                    item.version,
                    item.loader.label(),
                    item.loader_version.as_deref().unwrap_or("")
                ),
            ));
            let mut row = div().flex().flex_wrap().gap_2();
            for (i, loader) in [
                Loader::Fabric,
                Loader::Forge,
                Loader::NeoForge,
                Loader::Quilt,
            ]
            .into_iter()
            .enumerate()
            {
                row = row.child(
                    Button::new(("component", i), loader.label())
                        .when(t.loader == Some(loader), |b| b.primary())
                        .disabled(busy)
                        .on_click(cx.listener(move |h, _, _, cx| {
                            h.content.loader = Some(loader);
                            h.get_content_versions(false, cx);
                        })),
                );
            }
            components = components.child(row);
            if t.builds.len() > 10 {
                components = components.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new("comp-prev", "上一页")
                                .disabled(t.build_page == 0 || busy)
                                .on_click(cx.listener(|h, _, _, cx| {
                                    h.content.build_page = h.content.build_page.saturating_sub(1);
                                    cx.notify();
                                })),
                        )
                        .child(kicker(
                            cx,
                            &format!("{} / {}", t.build_page + 1, t.builds.len().div_ceil(10)),
                        ))
                        .child(
                            Button::new("comp-next", "下一页")
                                .disabled((t.build_page + 1) * 10 >= t.builds.len() || busy)
                                .on_click(cx.listener(|h, _, _, cx| {
                                    h.content.build_page += 1;
                                    cx.notify();
                                })),
                        ),
                );
            }
            for (i, build) in t.builds.iter().enumerate().skip(t.build_page * 10).take(10) {
                let v = build.clone();
                components = components.child(
                    Button::new(("component-build", i), format!("应用 {build}"))
                        .disabled(busy)
                        .on_click(cx.listener(move |h, _, _, cx| {
                            h.change_component(v.clone(), cx);
                        })),
                );
            }
            page = page.child(components);
            let mut updates = panel(cx, "整合包更新").child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("check-pack", "检查在线更新")
                            .disabled(busy || (t.upstream.is_none() && t.archive_hash.is_none()))
                            .on_click(cx.listener(|h, _, _, cx| h.get_content_versions(true, cx))),
                    )
                    .child(
                        Button::new("local-update", "选择本地新版包")
                            .disabled(busy)
                            .on_click(cx.listener(|h, _, _, cx| h.pick_update(cx))),
                    ),
            );
            for (i, r) in t.releases.iter().enumerate() {
                let r = r.clone();
                let item = item.clone();
                updates = updates.child(
                    Button::new(("update-release", i), r.name.clone())
                        .disabled(busy)
                        .on_click(cx.listener(move |h, _, _, cx| {
                            h.install_pack(hub::Pack::UpdateOnline(r.clone(), item.clone()), cx)
                        })),
                );
            }
            for (i, p) in t.rollbacks.iter().enumerate() {
                let path = p.clone();
                let date = fs::metadata(p)
                    .and_then(|m| m.modified())
                    .ok()
                    .map(chrono::DateTime::<chrono::Local>::from)
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                updates = updates.child(
                    Button::new(
                        ("rollback", i),
                        format!(
                            "{} {date}",
                            if t.confirm.as_ref() == Some(p) {
                                "确认回滚至"
                            } else {
                                "回滚至"
                            }
                        ),
                    )
                    .disabled(busy)
                    .on_click(cx.listener(move |h, _, _, cx| h.rollback_pack(path.clone(), cx))),
                );
            }
            page = page.child(updates);
        }
        let mut groups = div().flex().flex_wrap().gap_2();
        for (i, (_, label)) in model::EXPORT_GROUPS.into_iter().enumerate() {
            groups = groups.child(
                Button::new(("export-group", i), label)
                    .when(t.groups[i], |b| b.primary())
                    .on_click(cx.listener(move |h, _, _, cx| {
                        h.content.groups[i] = !h.content.groups[i];
                        cx.notify();
                    })),
            );
        }
        page.child(
            panel(cx, "导出整合包")
                .child(groups)
                .child(kicker(cx, "选中的文件会打入包中，请确认允许分享这些文件。"))
                .child(
                    Button::new("export-pack", "导出 .mrpack")
                        .disabled(busy)
                        .on_click(cx.listener(|h, _, _, cx| h.save_archive(false, cx))),
                ),
        )
    }
}
