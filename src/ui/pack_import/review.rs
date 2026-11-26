use super::*;
use crate::content::review::Review;
impl Draft {
    pub(super) fn persist_options(&self) -> Result<()> {
        self.pack.job.as_ref().unwrap().change(|r| {
            r.optional = Some(
                self.optional
                    .iter()
                    .map(|i| self.pack.files[*i].path.clone())
                    .collect(),
            );
            r.choices = self.choices.clone();
        })
    }
}
impl Home {
    pub(super) fn save_options(&mut self, cx: &mut Context<Self>) {
        if let Some(d) = &self.draft
            && let Err(e) = d.persist_options()
        {
            self.message(Err(e), cx);
        }
    }
    pub(super) fn refresh_review(&mut self, cx: &mut Context<Self>) {
        let Some(d) = &mut self.draft else { return };
        if d.pack.target.is_none() || d.pack.metadata_error.is_some() {
            return;
        }
        d.review = None;
        d.review_error = None;
        d.review_revision += 1;
        let revision = d.review_revision;
        let pack = d.pack.clone();
        let opt = d.optional.clone();
        let root = self.root.clone();
        let task = cx.background_executor().spawn(async move {
            let result = pack.review(&root, &opt);
            (pack, result)
        });
        cx.spawn(async move |view, cx| {
            let (pack, result) = task.await;
            let _ = view.update(cx, |h, cx| {
                if let Some(d) = &mut h.draft
                    && Arc::ptr_eq(&pack, &d.pack)
                    && d.review_revision == revision
                {
                    match result {
                        Ok(r) => {
                            d.choices.retain(|p, c| r.conflicts.get(p) == Some(c));
                            d.review = Some(Arc::new(r));
                        }
                        Err(e) => d.review_error = Some(format!("{e:#}")),
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }
    pub(super) fn retry_one(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(d) = &self.draft else { return };
        if self.cancel.is_some() {
            return;
        }
        let pack = d.pack.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.tasks.track(&cancel);
        self.cancel = Some(cancel.clone());
        self.status = format!("重试 {}", pack.files[index].path);
        let task = cx.background_executor().spawn(async move {
            pack.retry_file(index, &cancel, &|_| {})?;
            pack.job.as_ref().unwrap().change(|r| r.failed = None)
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |h, cx| {
                h.cancel = None;
                match result {
                    Ok(()) => {
                        if let Some(d) = &mut h.draft {
                            d.failed_file = None;
                            d.supplied.insert(index);
                        }
                        h.failed = false;
                        h.status = "文件已校验，可以继续安装".into();
                    }
                    Err(e) => h.message(Err(e), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn review_panel(&self, d: &Draft, busy: bool, cx: &Context<Self>) -> Div {
        let mut card = panel(cx, "这次更新会改什么").child(
            Button::new("pack-diff-refresh", "重新检查")
                .disabled(busy)
                .on_click(cx.listener(|h, _, _, cx| h.refresh_review(cx))),
        );
        let Some(r) = &d.review else {
            return card.child(kicker(
                cx,
                d.review_error.as_deref().unwrap_or("正在比较新旧整合包…"),
            ));
        };
        if r.legacy {
            card = card.child(kicker(cx, "此实例缺少旧版清单，以下列出新版提供的模组。"));
        }
        let summary = if r.legacy {
            format!("新版包含 {} 个模组文件", r.added.len())
        } else {
            format!(
                "模组  ·  新增 {}  /  更新 {}  /  移除 {}",
                r.added.len(),
                r.changed.len(),
                r.removed.len()
            )
        };
        card = card.child(summary).child(
            Button::new(
                "pack-diff-details",
                if d.detail_open {
                    "收起模组明细"
                } else {
                    "查看模组明细"
                },
            )
            .on_click(cx.listener(|h, _, _, cx| {
                if let Some(d) = &mut h.draft {
                    d.detail_open = !d.detail_open;
                }
                cx.notify();
            })),
        );
        if d.detail_open {
            let Review {
                added,
                changed,
                removed,
                ..
            } = r.as_ref();
            for (kind, paths) in [
                (if r.legacy { "新版提供" } else { "新增" }, added),
                ("更新", changed),
                ("移除", removed),
            ] {
                for path in paths {
                    card = card.child(kicker(cx, &format!("{kind}  ·  {path}")));
                }
            }
        }
        if !r.conflicts.is_empty() {
            card = card.child(format!("你改过的配置  ·  {}", r.conflicts.len()));
        }
        for (i, (path, conflict)) in r.conflicts.iter().enumerate() {
            let prefer = d.choices.contains_key(path);
            let mut row = div().flex().gap_2().flex_wrap().items_center().child(
                div()
                    .flex_1()
                    .child(path.clone())
                    .child(kicker(cx, conflict.note())),
            );
            for (j, (value, label)) in [(false, "保留我的"), (true, "使用新版")]
                .into_iter()
                .enumerate()
            {
                let path = path.clone();
                let conflict = conflict.clone();
                row = row.child(
                    Button::new(("pack-config-choice", 2 * i + j), label)
                        .when(prefer == value, |b| b.primary())
                        .disabled(busy)
                        .on_click(cx.listener(move |h, _, _, cx| {
                            if let Some(d) = &mut h.draft {
                                if value {
                                    d.choices.insert(path.clone(), conflict.clone());
                                } else {
                                    d.choices.remove(&path);
                                }
                            }
                            h.save_options(cx);
                            cx.notify();
                        })),
                );
            }
            card = card.child(row);
        }
        card
    }
}
