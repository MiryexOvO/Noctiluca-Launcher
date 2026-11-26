use super::*;
use crate::mods::local::{self, Filter, LocalMod};
use ely_gpui_component::forms::InputEvent;

pub(super) struct LocalMods {
    query: Entity<TextInput>,
    _subscription: Subscription,
    filter: Filter,
    page: usize,
    rows: Vec<LocalMod>,
    pub revision: u64,
    loading: bool,
    pub checked: bool,
    pub online: bool,
    pending: Option<crate::mods::dependents::Toggle>,
}

impl LocalMods {
    pub fn new(window: &mut Window, cx: &mut Context<Home>) -> Self {
        let query = cx.new(|cx| {
            TextInput::new(window, cx)
                .label("搜索已安装模组")
                .placeholder("模组名、模组 ID 或 JAR 文件名")
                .max_len(180)
        });
        let subscription = cx.subscribe(&query, |h: &mut Home, _, event: &InputEvent, cx| {
            if *event == InputEvent::Changed {
                h.local_mods.page = 0;
                cx.notify();
            }
        });
        Self {
            query,
            _subscription: subscription,
            filter: Filter::All,
            page: 0,
            rows: vec![],
            revision: 0,
            loading: false,
            checked: false,
            online: false,
            pending: None,
        }
    }
}

impl Home {
    pub(super) fn open_mods(&mut self, cx: &mut Context<Self>) {
        self.page = Page::Mods;
        self.local_mods.online = false;
        self.hits.clear();
        self.content.curse = crate::preferences::current().community == 1;
        self.local_mods.filter = Filter::All;
        self.local_mods
            .query
            .update(cx, |input, cx| input.set_text("", cx));
        self.refresh_mods(cx);
    }

    pub(super) fn refresh_mods(&mut self, cx: &mut Context<Self>) {
        self.local_mods.revision += 1;
        self.local_mods.page = 0;
        let revision = self.local_mods.revision;
        self.local_mods.loading = true;
        self.local_mods.checked = false;
        self.local_mods.rows.clear();
        self.local_mods.pending = None;
        self.content.upgrades.clear();
        let item = self.instances[self.selected].clone();
        let id = item.id.clone();
        let root = self.root.clone();
        let task = cx
            .background_executor()
            .spawn(async move { local::inventory(&item.game_dir(&root)?.join("mods")) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |h, cx| {
                if h.local_mods.revision != revision
                    || h.page != Page::Mods
                    || h.instances.get(h.selected).is_none_or(|i| i.id != id)
                {
                    return;
                }
                h.local_mods.loading = false;
                match result {
                    Ok(rows) => h.local_mods.rows = rows,
                    Err(e) => h.message(Err(e), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn toggle_mod(
        &mut self,
        name: String,
        confirmed: Option<mods::dependents::Toggle>,
        cx: &mut Context<Self>,
    ) {
        if self.copying {
            return;
        }
        self.copying = true;
        self.local_mods.pending = None;
        let root = self.root.clone();
        let item = self.instances[self.selected].clone();
        let id = item.id.clone();
        let task = cx.background_executor().spawn(async move {
            let plan = match confirmed {
                Some(p) => p,
                None => {
                    let p = mods::dependents::inspect(
                        &item.game_dir(&root)?.join("mods"),
                        item.loader,
                        &name,
                    )?;
                    if !p.affected.is_empty() {
                        return anyhow::Ok(Some(p));
                    }
                    p
                }
            };
            mods::dependents::apply(&root, &item, &plan)?;
            Ok(None)
        });
        cx.spawn(async move |view, cx| {
            let r = task.await;
            let _ = view.update(cx, |h, cx| {
                h.copying = false;
                if h.page == Page::Mods && h.instances.get(h.selected).is_some_and(|i| i.id == id) {
                    match r {
                        Ok(Some(p)) => h.local_mods.pending = Some(p),
                        Ok(None) => h.refresh_mods(cx),
                        Err(e) => h.message(Err(e), cx),
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn installed_mods(&self, cx: &Context<Self>) -> Div {
        let local = &self.local_mods;
        let busy = self.searching || self.copying || self.cancel.is_some();
        let mut filters = div().flex().flex_wrap().gap_1();
        for (i, (label, filter)) in [
            ("全部", Filter::All),
            ("已启用", Filter::Enabled),
            ("已停用", Filter::Disabled),
            ("有更新", Filter::Updates),
        ]
        .into_iter()
        .enumerate()
        {
            filters = filters.child(
                Button::new(("local-mod-filter", i), label)
                    .when(local.filter == filter, |b| b.primary())
                    .on_click(cx.listener(move |h, _, _, cx| {
                        h.local_mods.page = 0;
                        h.local_mods.filter = filter;
                        if filter == Filter::Updates
                            && !h.local_mods.checked
                            && !h.searching
                            && h.cancel.is_none()
                        {
                            h.check_mod_updates(cx);
                        }
                        cx.notify();
                    })),
            );
        }
        let query = local.query.read(cx).text().trim().to_lowercase();
        let updates: std::collections::HashMap<_, _> = self
            .content
            .upgrades
            .iter()
            .map(|u| (u.old.as_str(), u))
            .collect();
        let matches: Vec<_> = local
            .rows
            .iter()
            .filter(|m| m.matches(&query, local.filter, updates.contains_key(m.file.as_str())))
            .collect();
        let mut list = panel(
            cx,
            &format!("已安装模组  ·  {} / {}", matches.len(), local.rows.len()),
        )
        .child(Input::new(&local.query))
        .child(filters);
        if let Some(plan) = &local.pending {
            let names = plan
                .affected
                .iter()
                .map(|(f, n)| {
                    if n.is_empty() {
                        f.clone()
                    } else {
                        format!("{n}（{f}）")
                    }
                })
                .collect::<Vec<_>>()
                .join("、");
            list = list
                .child(kicker(
                    cx,
                    &format!("停用 {} 也会影响以下已启用的模组：{}", plan.name, names),
                ))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(Button::new("cancel-toggle", "取消").on_click(cx.listener(
                            |h, _, _, cx| {
                                h.local_mods.pending = None;
                                cx.notify();
                            },
                        )))
                        .child(
                            Button::new(
                                "cascade-toggle",
                                format!("一起停用 {} 个模组", 1 + plan.affected.len()),
                            )
                            .disabled(busy)
                            .on_click(cx.listener(|h, _, _, cx| {
                                if let Some(plan) = h.local_mods.pending.take() {
                                    h.toggle_mod(plan.name.clone(), Some(plan), cx);
                                }
                            })),
                        ),
                );
        }
        if local.filter == Filter::Updates && self.content.upgrades.len() > 1 {
            list = list.child(
                Button::new(
                    "mod-upgrade-all",
                    format!("更新全部 {} 个模组", self.content.upgrades.len()),
                )
                .disabled(busy)
                .on_click(
                    cx.listener(|h, _, _, cx| h.apply_mod_update(h.content.upgrades.clone(), cx)),
                ),
            );
        }
        if local.loading {
            return list.child(kicker(cx, "正在读取本地模组…"));
        }
        if local.filter == Filter::Updates && !local.checked {
            return list.child(kicker(
                cx,
                if self.searching {
                    "正在检查更新…"
                } else {
                    "点「检查已装模组更新」获取当前平台的结果"
                },
            ));
        }
        if matches.is_empty() {
            return list.child(kicker(
                cx,
                if local.rows.is_empty() {
                    "这个实例还没有安装模组"
                } else {
                    "没有符合当前条件的模组"
                },
            ));
        }
        let filenames = crate::preferences::current().name_style != 0;
        const PER_PAGE: usize = 40;
        let pages = matches.len().div_ceil(PER_PAGE);
        let page = local.page.min(pages - 1);
        for (idx, m) in matches
            .into_iter()
            .enumerate()
            .skip(page * PER_PAGE)
            .take(PER_PAGE)
        {
            let label = m.label();
            let mut text = div().flex_1().min_w_0().flex().flex_col();
            if !filenames && !label.is_empty() {
                text = text
                    .child(div().truncate().child(label))
                    .child(kicker(cx, &m.file).truncate());
            } else {
                text = text.child(div().truncate().child(m.file.clone()));
                if filenames && !label.is_empty() {
                    text = text.child(kicker(cx, &label).truncate());
                }
            }
            let name = m.file.clone();
            let update = updates.get(name.as_str());
            if let Some(u) = update {
                text = text.child(kicker(cx, &format!("可更新至 {}", u.new)).truncate());
            }
            let mut row = div().flex().items_center().gap_2().child(text);
            if let Some(u) = update {
                let u = (**u).clone();
                row = row.child(
                    Button::new(("local-upgrade", idx), "更新")
                        .disabled(busy)
                        .on_click(
                            cx.listener(move |h, _, _, cx| h.apply_mod_update(vec![u.clone()], cx)),
                        ),
                );
            }
            list = list.child(
                row.child(
                    Button::new(
                        ("toggle-mod", idx),
                        if m.disabled() { "启用" } else { "停用" },
                    )
                    .disabled(busy)
                    .on_click(cx.listener(move |h, _, _, cx| {
                        h.toggle_mod(name.clone(), None, cx);
                    })),
                ),
            );
        }
        if pages > 1 {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("mod-list-prev", "上一页")
                            .disabled(page == 0)
                            .on_click(cx.listener(move |h, _, _, cx| {
                                h.local_mods.page = page.saturating_sub(1);
                                cx.notify();
                            })),
                    )
                    .child(kicker(cx, &format!("{} / {pages}", page + 1)))
                    .child(
                        Button::new("mod-list-next", "下一页")
                            .disabled(page + 1 >= pages)
                            .on_click(cx.listener(move |h, _, _, cx| {
                                h.local_mods.page = (page + 1).min(pages - 1);
                                cx.notify();
                            })),
                    ),
            );
        }
        list
    }
}
