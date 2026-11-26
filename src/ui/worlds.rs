use super::*;
use crate::worlds::{self, World};
use chrono::Local;

#[derive(Default)]
pub(super) struct WorldList {
    request: u64,
    loading: bool,
    rows: Vec<(World, Option<Arc<Image>>)>,
    offset: usize,
    error: Option<String>,
}

impl Home {
    pub(super) fn read_worlds(&mut self, cx: &mut Context<Self>) {
        self.worlds.request += 1;
        let request = self.worlds.request;
        self.worlds.loading = true;
        self.worlds.rows.clear();
        self.worlds.error = None;
        self.worlds.offset = 0;
        let item = self.instances[self.selected].clone();
        let id = item.id.clone();
        let root = self.root.clone();
        let task = cx.background_executor().spawn(async move {
            let saves = crate::content::resource_dir(&root, &item, "saves")?;
            let worlds = worlds::list(&saves)?;
            Ok::<_, anyhow::Error>(
                worlds
                    .into_iter()
                    .map(|world| {
                        let icon = worlds::thumbnail(&saves.join(&world.folder))
                            .map(|b| Arc::new(Image::from_bytes(ImageFormat::Png, b)));
                        (world, icon)
                    })
                    .collect(),
            )
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |h, cx| {
                if h.page != Page::Files
                    || h.content.kind != "saves"
                    || h.worlds.request != request
                    || h.instances.get(h.selected).is_none_or(|i| i.id != id)
                {
                    return;
                }
                h.worlds.loading = false;
                match result {
                    Ok(rows) => h.worlds.rows = rows,
                    Err(e) => h.worlds.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn backup_world(&mut self, folder: std::ffi::OsString, cx: &mut Context<Self>) {
        if self.copying || self.cancel.is_some() {
            return;
        }
        let item = self.instances[self.selected].clone();
        let root = self.root.clone();
        self.copying = true;
        self.failed = false;
        self.status = format!("正在备份存档「{}」", folder.to_string_lossy());
        let task = cx
            .background_executor()
            .spawn(async move { worlds::backup(&root, &item, &folder) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |h, cx| {
                h.copying = false;
                match result {
                    Ok(p) => {
                        h.failed = false;
                        h.status = format!("世界备份已保存到 {}", p.display());
                    }
                    Err(e) => h.message(Err(e), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn world_rows(&self, page: Div, actions: Div, cx: &Context<Self>) -> Div {
        let mut page = page.child(actions.child(
            Button::new("world-backup-folder", "打开备份目录").on_click(cx.listener(
                |h, _, _, cx| {
                    let result = worlds::backup_dir(&h.root, &h.instances[h.selected])
                        .and_then(|p| crate::content::explore(&p));
                    h.message(result, cx);
                },
            )),
        ));
        let state = &self.worlds;
        if state.loading {
            return page.child(kicker(cx, "正在读取世界…"));
        }
        if let Some(error) = &state.error {
            return page.child(kicker(cx, error));
        }
        if state.rows.is_empty() {
            return page.child(kicker(cx, "这个实例还没有世界存档"));
        }
        let running = self.running.contains_key(&self.instances[self.selected].id);
        if running {
            page = page.child(kicker(cx, "退出游戏后即可备份存档。"));
        }
        for (i, (w, image)) in state.rows.iter().enumerate().skip(state.offset).take(40) {
            let folder = w.folder.clone();
            let played = w
                .played
                .map(|d| d.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "未知".into());
            let mut cover = div()
                .size(px(48.))
                .flex_none()
                .rounded_md()
                .overflow_hidden()
                .bg(cx.theme().colors.active)
                .flex()
                .items_center()
                .justify_center();
            cover = if let Some(image) = image {
                cover.child(
                    img(image.clone())
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                )
            } else {
                cover.child(Icon::new(IconName::Map).color(cx.theme().colors.accent))
            };
            let mut info = div().flex_1().min_w_0().flex().flex_col().gap_1().child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(w.name.clone()),
            );
            if w.name != w.folder.to_string_lossy() {
                info = info.child(kicker(cx, &w.folder.to_string_lossy()).truncate());
            }
            page = page.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded_md()
                    .bg(cx.theme().colors.surface)
                    .p_3()
                    .child(cover)
                    .child(info.child(kicker(cx, &format!("上次游玩  ·  {played}"))))
                    .child(
                        Button::new(("world-backup", i), "备份")
                            .disabled(self.copying || self.cancel.is_some() || running)
                            .on_click(
                                cx.listener(move |h, _, _, cx| h.backup_world(folder.clone(), cx)),
                            ),
                    ),
            );
        }
        if state.rows.len() > 40 {
            page = page.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("world-prev", "上一页")
                            .disabled(state.offset == 0)
                            .on_click(cx.listener(|h, _, _, cx| {
                                h.worlds.offset = h.worlds.offset.saturating_sub(40);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("world-next", "下一页")
                            .disabled(state.offset + 40 >= state.rows.len())
                            .on_click(cx.listener(|h, _, _, cx| {
                                h.worlds.offset += 40;
                                cx.notify();
                            })),
                    ),
            );
        }
        page
    }
}
