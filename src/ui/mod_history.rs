use super::*;
use crate::mods::history::{self, Change};
use uuid::Uuid;
#[derive(Default)]
pub(super) struct View {
    rows: Vec<Change>,
    expanded: Option<Uuid>,
    confirm: Option<Uuid>,
    loading: bool,
}
impl Home {
    pub(super) fn open_mod_history(&mut self, cx: &mut Context<Self>) {
        self.page = Page::ModHistory;
        self.changes = View {
            loading: true,
            ..Default::default()
        };
        let root = self.root.clone();
        let item = self.instances[self.selected].clone();
        let id = item.id.clone();
        let task = cx
            .background_executor()
            .spawn(async move { history::list(&root, &item) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |h, cx| {
                if h.page != Page::ModHistory
                    || h.instances.get(h.selected).is_none_or(|i| i.id != id)
                {
                    return;
                }
                h.changes.loading = false;
                match result {
                    Ok(rows) => h.changes.rows = rows,
                    Err(e) => h.message(Err(e), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn undo_mod_change(&mut self, change: Uuid, cx: &mut Context<Self>) {
        if self.copying {
            return;
        }
        let root = self.root.clone();
        let item = self.instances[self.selected].clone();
        let id = item.id.clone();
        self.copying = true;
        let task = cx
            .background_executor()
            .spawn(async move { history::undo(&root, &item, change) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |h, cx| {
                h.copying = false;
                if h.page == Page::ModHistory
                    && h.instances.get(h.selected).is_some_and(|i| i.id == id)
                {
                    match result {
                        Ok(()) => {
                            h.status = "已撤销这次模组改动".into();
                            h.open_mod_history(cx)
                        }
                        Err(e) => {
                            h.changes.confirm = None;
                            h.message(Err(e), cx)
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn mod_history_page(&self, cx: &Context<Self>) -> Div {
        let item = &self.instances[self.selected];
        let mut page = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(kicker(cx, &item.name))
            .child(heading("最近改动"));
        if self.changes.loading {
            page = page.child(kicker(cx, "正在读取改动…"));
        } else if self.changes.rows.is_empty() {
            page = page.child(kicker(cx, "这个实例还没有模组改动记录。"));
        }
        let latest = self.changes.rows.iter().find(|r| !r.undone).map(|r| r.id);
        for (i, c) in self.changes.rows.iter().enumerate() {
            let id = c.id;
            let time = chrono::DateTime::from_timestamp_micros(c.time)
                .map(|t| {
                    t.with_timezone(&chrono::Local)
                        .format("%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default();
            let mut row = panel(cx, &c.title).child(kicker(
                cx,
                &format!(
                    "{time}  ·  {} 个文件{}",
                    c.files.len(),
                    if c.undone { "  ·  已撤销" } else { "" }
                ),
            ));
            let open = self.changes.expanded == Some(id);
            let mut actions = div().flex().gap_2().child(
                Button::new(
                    ("change-details", i),
                    if open { "收起" } else { "查看文件" },
                )
                .on_click(cx.listener(move |h, _, _, cx| {
                    h.changes.expanded = if open { None } else { Some(id) };
                    cx.notify();
                })),
            );
            if latest == Some(id) {
                if self.changes.confirm == Some(id) {
                    row = row.child(kicker(
                        cx,
                        "将恢复这批模组的文件和启用状态。存档与配置会保留。",
                    ));
                    actions = actions
                        .child(
                            Button::new("undo-confirm", "确认撤销")
                                .disabled(self.copying)
                                .on_click(
                                    cx.listener(move |h, _, _, cx| h.undo_mod_change(id, cx)),
                                ),
                        )
                        .child(Button::new("undo-cancel", "取消").on_click(cx.listener(
                            |h, _, _, cx| {
                                h.changes.confirm = None;
                                cx.notify();
                            },
                        )));
                } else {
                    actions = actions.child(
                        Button::new("undo-change", "撤销这次改动")
                            .disabled(self.copying || self.running.contains_key(&item.id))
                            .on_click(cx.listener(move |h, _, _, cx| {
                                h.changes.confirm = Some(id);
                                h.changes.expanded = Some(id);
                                cx.notify();
                            })),
                    );
                }
            }
            row = row.child(actions);
            if open {
                for f in &c.files {
                    let labels = match (f.old_label.as_str(), f.new_label.as_str()) {
                        ("", s) | (s, "") => s.to_owned(),
                        (a, b) => format!("{a} → {b}"),
                    };
                    row = row.child(kicker(
                        cx,
                        &format!("{}  ·  {}  {}", f.action(), f.name, labels),
                    ));
                }
            }
            page = page.child(row);
        }
        page.child(
            Button::new("back-from-mod-history", "返回实例").on_click(cx.listener(
                |h, _, _, cx| {
                    h.page = Page::Detail;
                    cx.notify();
                },
            )),
        )
    }
}
