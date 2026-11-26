use super::*;
use crate::local_resources::{self, Drop, Kind};

impl Home {
    pub(super) fn inspect_local_drop(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.copying {
            return;
        }
        let root = self.root.clone();
        let item = self.instances[self.selected].clone();
        let id = item.id.clone();
        let page = self.page;
        let items = self.instances.clone();
        self.copying = true;
        self.failed = false;
        self.status = format!("正在检查拖入「{}」的文件", item.name);
        let task = cx
            .background_executor()
            .spawn(async move { local_resources::preview(&root, &item, &paths, &items) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |h, cx| {
                h.copying = false;
                if h.page != page || h.instances.get(h.selected).is_none_or(|i| i.id != id) {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(Drop::Pack(path)) => h.install_pack(hub::Pack::Dropped(path), cx),
                    Ok(Drop::Files(plan)) => {
                        h.local_drop = Some(*plan);
                        h.page = Page::LocalResources;
                        h.status = "请确认要安装的本地资源".into();
                    }
                    Err(e) => h.message(Err(e), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn install_local_drop(&mut self, cx: &mut Context<Self>) {
        let Some(plan) = self.local_drop.clone() else {
            return;
        };
        if self.copying
            || self.cancel.is_some()
            || self
                .instances
                .get(self.selected)
                .is_none_or(|i| i.id != plan.item.id)
        {
            return;
        }
        self.copying = true;
        self.failed = false;
        self.status = "正在写入实例目录".into();
        let root = self.root.clone();
        let item = plan.item.clone();
        let task = cx
            .background_executor()
            .spawn(async move { plan.apply(&root) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |h, cx| {
                h.copying = false;
                match result {
                    Ok(n) => {
                        h.local_drop = None;
                        if h.page == Page::LocalResources {
                            h.page = Page::Detail;
                        }
                        h.status = format!("已为「{}」安装 {n} 个资源", item.name);
                        h.failed = false;
                    }
                    Err(e) => h.message(Err(e), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn local_drop_page(&self, cx: &Context<Self>) -> Div {
        let mut page = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(heading("导入本地资源"));
        let Some(plan) = &self.local_drop else {
            return page;
        };
        let busy = self.copying || self.cancel.is_some();
        page = page.child(kicker(
            cx,
            &format!(
                "目标实例  ·  {}   /   {} {}",
                plan.item.name,
                plan.item.version,
                plan.item.loader.label()
            ),
        ));
        for (kind, names) in &plan.sharing {
            if plan.rows.iter().any(|r| r.kind == Some(*kind) && !r.skip) {
                page = page.child(kicker(
                    cx,
                    &format!(
                        "与「{}」共用{}目录，安装后这些实例也会读到此文件。",
                        names.join("」、「"),
                        kind.label()
                    ),
                ));
            }
        }
        for (i, row) in plan.rows.iter().enumerate() {
            let mut buttons = div().flex().flex_wrap().gap_2();
            if row.ambiguous {
                for (j, kind) in [Kind::Resource, Kind::Shader].into_iter().enumerate() {
                    buttons = buttons.child(
                        Button::new(("drop-kind", i * 2 + j), kind.label())
                            .when(row.kind == Some(kind), |b| b.primary())
                            .disabled(busy)
                            .on_click(cx.listener(move |h, _, _, cx| {
                                if let Some(p) = &mut h.local_drop {
                                    p.rows[i].select(kind);
                                }
                                cx.notify();
                            })),
                    );
                }
            }
            let enabled = !row.skip;
            buttons = buttons
                .child(
                    Button::new(("drop-skip", i), "跳过")
                        .when(!enabled, |b| b.primary())
                        .disabled(busy)
                        .on_click(cx.listener(move |h, _, _, cx| {
                            if let Some(p) = &mut h.local_drop {
                                p.rows[i].skip = true;
                                p.rows[i].replace = false;
                            }
                            cx.notify();
                        })),
                )
                .child(
                    Button::new(
                        ("drop-keep", i),
                        if row.conflict() {
                            "替换旧文件"
                        } else {
                            "安装"
                        },
                    )
                    .when(enabled && row.kind.is_some(), |b| b.primary())
                    .disabled(busy || row.kind.is_none() || row.blocked())
                    .on_click(cx.listener(move |h, _, _, cx| {
                        if let Some(p) = &mut h.local_drop {
                            let r = &mut p.rows[i];
                            r.skip = false;
                            r.replace = r.conflict();
                        }
                        cx.notify();
                    })),
                );
            let kind = row.kind.map_or("请选择 ZIP 类型", Kind::label);
            let desc = if row.conflict() {
                format!("{kind}  ·  已有匹配文件")
            } else {
                kind.to_string()
            };
            page = page.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .bg(cx.theme().colors.surface)
                    .child(row.name.clone())
                    .child(kicker(cx, &desc))
                    .when_some(
                        row.check.as_ref().filter(|c| !c.note.is_empty()),
                        |el, c| el.child(kicker(cx, &c.note)),
                    )
                    .child(buttons),
            );
        }
        let selection = plan.selection();
        let label = match &selection {
            Ok(n) => format!("安装 {n} 个文件"),
            Err(e) => {
                page = page.child(kicker(cx, &e.to_string()));
                "安装".into()
            }
        };
        let running = self.running.contains_key(&plan.item.id);
        if running {
            page = page.child(kicker(cx, "退出此实例后即可安装。"));
        }
        page.child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(
                    Button::new("drop-apply", label)
                        .primary()
                        .disabled(busy || running || selection.is_err())
                        .on_click(cx.listener(|h, _, _, cx| h.install_local_drop(cx))),
                )
                .child(
                    Button::new("drop-refresh", "重新检查")
                        .disabled(busy)
                        .on_click(cx.listener(|h, _, _, cx| {
                            if let Some(p) = &h.local_drop {
                                h.inspect_local_drop(
                                    p.rows.iter().map(|r| r.path.clone()).collect(),
                                    cx,
                                );
                            }
                        })),
                )
                .child(
                    Button::new("drop-back", "取消")
                        .disabled(busy)
                        .on_click(cx.listener(|h, _, _, cx| {
                            h.local_drop = None;
                            h.page = Page::Detail;
                            cx.notify();
                        })),
                ),
        )
    }
}
