use super::*;
use crate::packs::jobs::{self, Job, Record};
use std::{path::Path, sync::Weak, time::Duration};

pub(super) struct Shelf {
    pub active: Option<Arc<Job>>,
    pub expanded: bool,
    pub records: Vec<Record>,
    pub error: Option<String>,
    worker: Weak<AtomicBool>,
    window: AnyWindowHandle,
    pub closing: bool,
}
impl Shelf {
    pub fn new(root: &Path, window: AnyWindowHandle) -> Self {
        let mut shelf = Self {
            active: None,
            expanded: false,
            records: vec![],
            error: None,
            worker: Weak::new(),
            window,
            closing: false,
        };
        shelf.read(root);
        shelf.expanded = !shelf.records.is_empty() || shelf.error.is_some();
        shelf
    }
    fn read(&mut self, root: &Path) {
        match jobs::list(root) {
            Ok(jobs) => {
                self.records = jobs;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }
    pub fn track(&mut self, cancel: &Arc<AtomicBool>) {
        self.worker = Arc::downgrade(cancel);
    }
    pub fn is_working(&self, cancel: Option<&Arc<AtomicBool>>) -> bool {
        cancel.is_some_and(|c| Weak::ptr_eq(&self.worker, &Arc::downgrade(c)))
    }
}
impl Home {
    pub(super) fn reload_jobs(&mut self, cx: &mut Context<Self>) {
        self.tasks.read(&self.root);
        cx.notify();
    }
    pub(super) fn pause_pack_on_close(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.tasks.is_working(self.cancel.as_ref()) {
            return false;
        }
        if self.tasks.closing {
            return true;
        }
        self.tasks.closing = true;
        self.cancel.as_ref().unwrap().store(true, Ordering::Release);
        self.status = "正在暂停整合包任务，保存后关闭窗口".into();
        let window = self.tasks.window;
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                match view.update(cx, |h, _| h.cancel.is_none() && !h.copying) {
                    Ok(true) => {
                        let _ = window.update(cx, |_, w, _| w.remove_window());
                        break;
                    }
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
        })
        .detach();
        cx.notify();
        true
    }
    fn discard_pack_task(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.cancel.is_some() {
            return;
        }
        let active = self
            .tasks
            .active
            .as_ref()
            .filter(|j| j.record().id == id)
            .cloned();
        let result = active
            .clone()
            .map_or_else(|| Job::open(&self.root, id), Ok)
            .and_then(|j| j.finish());
        if let Err(e) = result {
            self.message(Err(e), cx);
            return;
        }
        if active.is_some() {
            self.draft = None;
            self.tasks.active = None;
        }
        drop(active);
        self.reload_jobs(cx);
    }
    pub(super) fn task_shelf(&self, cx: &Context<Self>) -> Div {
        let mut shelf = div().flex().flex_col().gap_1().child(
            Button::new(
                "pack-tasks-toggle",
                format!("下载任务（{}）", self.tasks.records.len()),
            )
            .on_click(cx.listener(|h, _, _, cx| {
                h.tasks.expanded = !h.tasks.expanded;
                h.reload_jobs(cx);
            })),
        );
        if !self.tasks.expanded {
            return shelf;
        }
        if let Some(error) = &self.tasks.error {
            shelf = shelf.child(kicker(cx, error));
        }
        let mut rows = div()
            .id("pack-task-rows")
            .max_h(px(170.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();
        for (i, record) in self.tasks.records.iter().enumerate() {
            let id = record.id.clone();
            let remove = id.clone();
            let current = self
                .tasks
                .active
                .as_ref()
                .is_some_and(|j| j.record().id == id);
            let working = current && self.tasks.is_working(self.cancel.as_ref());
            let preview = current && self.draft.is_some();
            let mut row = div().flex().gap_2().items_center().child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .child(record.title.clone()),
            );
            if let Some(d) = self.draft.as_ref().filter(|_| current) {
                let left = d.pack.estimate(&d.optional);
                let manual = (0..d.pack.files.len()).filter(|&i| d.missing(i)).count();
                let size = format_bytes(left.pending_bytes);
                row = row.child(kicker(
                    cx,
                    &format!(
                        "剩余 {} 个 · {}{} · 手动补 {} 个",
                        left.pending_count,
                        if left.unknown_count > 0 {
                            "至少 "
                        } else {
                            ""
                        },
                        size,
                        manual
                    ),
                ));
            }
            if !working || preview {
                let view_only = working || (preview && !record.confirmed && !record.prefetch);
                row = row.child(
                    Button::new(
                        ("open-pack-task", i),
                        if working {
                            "查看进度"
                        } else if view_only {
                            "查看任务"
                        } else {
                            "继续安装"
                        },
                    )
                    .disabled(
                        (self.cancel.is_some() && !working) || self.copying || self.logging_in,
                    )
                    .on_click(cx.listener(move |h, _, _, cx| {
                        h.tasks.expanded = false;
                        if view_only {
                            h.page = Page::Import;
                            cx.notify();
                        } else {
                            h.install_pack(hub::Pack::Resume(id.clone()), cx);
                        }
                    })),
                );
            }
            if working {
                row = row.child(Button::new(("pause-pack", i), "暂停").on_click(cx.listener(
                    |h, _, _, cx| {
                        if let Some(c) = &h.cancel {
                            c.store(true, Ordering::Release);
                        }
                        h.status = "正在暂停任务".into();
                        cx.notify();
                    },
                )));
            } else {
                row = row.child(
                    Button::new(("remove-pack-task", i), "移除任务")
                        .disabled(self.cancel.is_some())
                        .on_click(cx.listener(move |h, _, _, cx| h.discard_pack_task(&remove, cx))),
                );
            }
            rows = rows.child(row);
            if let Some(d) = self.draft.as_ref().filter(|_| working) {
                let active = d.pack.active_files.lock().unwrap();
                if !active.is_empty() {
                    rows = rows.child(kicker(
                        cx,
                        &format!(
                            "正在获取 {}",
                            active
                                .iter()
                                .take(3)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("、")
                        ),
                    ));
                }
            }
        }
        if self.tasks.records.is_empty() {
            rows = rows.child(kicker(cx, "没有待继续的整合包任务"));
        }
        shelf.child(rows)
    }
}
