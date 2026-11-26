use super::*;
use crate::organization::{Entry, Organization};
pub(super) struct Organizer {
    pub data: Organization,
    field: Entity<TextInput>,
    pub filter: Option<String>,
}
impl Organizer {
    pub fn new(root: &std::path::Path, window: &mut Window, cx: &mut Context<Home>) -> Self {
        Self {
            data: Organization::read(root).unwrap_or_default(),
            field: cx.new(|cx| {
                TextInput::new(window, cx)
                    .label("实例分组")
                    .placeholder("例如：与朋友联机")
                    .max_len(50)
            }),
            filter: None,
        }
    }
}
impl Home {
    pub(super) fn open_organize(&mut self, cx: &mut Context<Self>) {
        let group = self
            .organizer
            .data
            .group(&self.instances[self.selected].id)
            .to_owned();
        self.organizer
            .field
            .update(cx, |f, cx| f.set_text(group, cx));
        self.page = Page::Organize;
        cx.notify();
    }
    fn edit_entry(&mut self, edit: impl FnOnce(&mut Entry), cx: &mut Context<Self>) {
        match Organization::edit(&self.root, &self.instances[self.selected].id, edit) {
            Ok(data) => {
                self.organizer.data = data;
                self.failed = false;
                self.status = "实例库已更新".into();
            }
            Err(e) => self.message(Err(e), cx),
        }
        if self.organizer.filter.as_ref().is_some_and(|v| {
            !v.is_empty() && !self.organizer.data.groups(&self.instances).contains(v)
        }) {
            self.organizer.filter = None;
        }
        cx.notify();
    }
    fn assign_group(&mut self, name: String, cx: &mut Context<Self>) {
        self.organizer
            .field
            .update(cx, |f, cx| f.set_text(name.clone(), cx));
        self.edit_entry(|e| e.group = name, cx);
    }
    fn make_shortcut(&mut self, cx: &mut Context<Self>) {
        if self.copying {
            return;
        }
        self.copying = true;
        self.failed = false;
        self.status = "正在创建快捷方式".into();
        let root = self.root.clone();
        let item = self.instances[self.selected].clone();
        let task = cx
            .background_executor()
            .spawn(async move { crate::shortcut::desktop(&root, &item) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |h, cx| {
                h.copying = false;
                match result {
                    Ok(p) => {
                        h.status = format!("桌面快捷方式已保存到 {}", p.display());
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
    pub(super) fn organize_page(&self, cx: &Context<Self>) -> Div {
        let item = &self.instances[self.selected];
        let entry = self.organizer.data.get(&item.id);
        let group = self.organizer.data.group(&item.id);
        let mut tags = div().flex().flex_wrap().gap_1();
        for (i, name) in self
            .organizer
            .data
            .groups(&self.instances)
            .into_iter()
            .enumerate()
        {
            tags = tags.child(
                Button::new(("set-existing-group", i), name.clone())
                    .when(name == group, |b| b.primary())
                    .on_click(cx.listener(move |h, _, _, cx| h.assign_group(name.clone(), cx))),
            );
        }
        let date = entry
            .and_then(|e| {
                if e.played > 0 {
                    chrono::DateTime::from_timestamp_millis(e.played)
                } else {
                    None
                }
            })
            .map(|d| {
                d.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d  %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| "暂无游玩记录".into());
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(kicker(cx, &item.name))
            .child(heading("整理实例"))
            .child(
                panel(cx, "快捷操作")
                    .child(kicker(cx, &format!("最近游玩  ·  {date}")))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Button::new(
                                    "pin-instance",
                                    if entry.is_some_and(|e| e.pinned) {
                                        "取消置顶"
                                    } else {
                                        "置顶实例"
                                    },
                                )
                                .on_click(cx.listener(
                                    |h, _, _, cx| h.edit_entry(|e| e.pinned = !e.pinned, cx),
                                )),
                            )
                            .child(
                                Button::new("create-shortcut", "创建桌面快捷方式")
                                    .disabled(self.copying)
                                    .on_click(cx.listener(|h, _, _, cx| h.make_shortcut(cx))),
                            ),
                    ),
            )
            .child(
                panel(cx, "实例分组")
                    .child(kicker(
                        cx,
                        if group.is_empty() {
                            "当前未分组"
                        } else {
                            group
                        },
                    ))
                    .child(tags)
                    .child(Input::new(&self.organizer.field))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(Button::new("save-group", "保存分组").primary().on_click(
                                cx.listener(|h, _, _, cx| {
                                    let name = h.organizer.field.read(cx).text().trim().to_string();
                                    h.assign_group(name, cx);
                                }),
                            ))
                            .child(
                                Button::new("clear-group", "移出分组")
                                    .disabled(group.is_empty())
                                    .on_click(
                                        cx.listener(|h, _, _, cx| {
                                            h.assign_group(String::new(), cx)
                                        }),
                                    ),
                            ),
                    ),
            )
            .child(
                Button::new("organize-back", "返回实例").on_click(cx.listener(|h, _, _, cx| {
                    h.page = Page::Detail;
                    cx.notify();
                })),
            )
    }
    pub(super) fn group_filter(&self, cx: &Context<Self>) -> Div {
        let groups = self.organizer.data.groups(&self.instances);
        if groups.is_empty() && self.organizer.filter.is_none() {
            return div();
        }
        let mut row = div().flex().flex_wrap().gap_1();
        for (i, option) in std::iter::once(None)
            .chain(std::iter::once(Some(String::new())))
            .chain(groups.into_iter().map(Some))
            .enumerate()
        {
            let label = match &option {
                None => "全部分组",
                Some(s) if s.is_empty() => "未分组",
                Some(s) => s,
            };
            row = row.child(
                Button::new(("library-group", i), label.to_owned())
                    .when(option == self.organizer.filter, |b| b.primary())
                    .on_click(cx.listener(move |h, _, _, cx| {
                        h.organizer.filter = option.clone();
                        cx.notify();
                    })),
            );
        }
        row
    }
}
