use super::*;
use crate::memory::{self, Global, Mode};
use ely_gpui_component::forms::Slider;
pub(super) struct Controls {
    pub(super) global: Global,
    editing_global: bool,
    confirm: Option<String>,
    removed: Option<Vec<Instance>>,
}
impl Controls {
    pub(super) fn reset_selection(&mut self) {
        self.confirm = None;
        self.editing_global = false;
    }
    pub(super) fn new(root: &std::path::Path) -> Self {
        Self {
            global: Global::read(root).unwrap_or_default(),
            editing_global: false,
            confirm: None,
            removed: None,
        }
    }
}
impl Home {
    fn change_memory(
        &mut self,
        edit: impl FnOnce(&mut Instance, &mut Global),
        cx: &mut Context<Self>,
    ) {
        let mut item = self.instances[self.selected].clone();
        let mut g = self.controls.global;
        edit(&mut item, &mut g);
        let result = if self.controls.editing_global {
            g.save(&self.root)
        } else {
            library::update(&self.root, &item)
        };
        if result.is_ok() {
            self.instances[self.selected] = item;
            self.controls.global = g;
            self.failed = false;
            self.status = "内存设置已保存".into();
        }
        self.message(result, cx);
    }
    pub(super) fn memory_panel(&self, busy: bool, cx: &Context<Self>) -> Div {
        let item = &self.instances[self.selected];
        let editing = self.controls.editing_global;
        let g = self.controls.global;
        let mode = if editing {
            if g.automatic {
                Mode::Auto
            } else {
                Mode::Custom
            }
        } else {
            item.memory_mode
        };
        let ram = memory::physical();
        let mut source = item.clone();
        if editing {
            source.memory_mode = Mode::Global;
        }
        let (initial, max) = memory::effective(&source, g, ram.as_ref());
        let mut modes = div().flex().flex_wrap().gap_1();
        for (i, (m, label)) in [
            (Mode::Global, "跟随全局"),
            (Mode::Auto, "自动"),
            (Mode::Custom, "自定义"),
        ]
        .into_iter()
        .enumerate()
        {
            if editing && m == Mode::Global {
                continue;
            }
            modes = modes.child(
                Button::new(("memory-mode", i), label)
                    .when(mode == m, |b| b.primary())
                    .disabled(busy)
                    .on_click(cx.listener(move |h, _, _, cx| {
                        h.change_memory(
                            |p, g| {
                                if editing {
                                    g.automatic = m == Mode::Auto;
                                } else {
                                    p.memory_mode = m;
                                }
                            },
                            cx,
                        );
                    })),
            );
        }
        let mut card = panel(if editing {
            "全局游戏内存"
        } else {
            "游戏内存"
        })
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .flex_wrap()
                .gap_1()
                .child(modes)
                .child(
                    Button::new(
                        "global-heap",
                        if editing {
                            "返回实例设置"
                        } else {
                            "编辑全局设置"
                        },
                    )
                    .disabled(busy)
                    .on_click(cx.listener(|h, _, _, cx| {
                        h.controls.editing_global = !h.controls.editing_global;
                        cx.notify();
                    })),
                ),
        );
        let cap = ram
            .as_ref()
            .map(|r| r.total.clamp(1024, 32768) as u32 / 128 * 128)
            .unwrap_or(32768)
            .max(max);
        for (i, (label, value, lower, limit)) in [
            ("最大内存  -Xmx", max, 512, cap),
            ("初始内存  -Xms", initial, 128, max),
        ]
        .into_iter()
        .enumerate()
        {
            let view = cx.weak_entity();
            let slider = Slider::new(("heap-slider", i), f64::from(value))
                .range(f64::from(lower), f64::from(limit))
                .step(128.)
                .disabled(busy || mode != Mode::Custom)
                .on_change(move |v, _, cx| {
                    let _ = view.update(cx, |h, cx| {
                        h.change_memory(
                            |p, g| {
                                let v = v as u32;
                                if editing {
                                    if i == 0 {
                                        g.max = v;
                                        g.initial = g.initial.min(v);
                                    } else {
                                        g.initial = v;
                                    }
                                } else if i == 0 {
                                    p.memory_mib = v;
                                    p.initial_mib = p.initial_mib.min(v);
                                } else {
                                    p.initial_mib = v;
                                }
                            },
                            cx,
                        );
                    });
                });
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().w(px(120.)).text_sm().child(label))
                    .child(div().flex_1().child(slider))
                    .child(div().w(px(80.)).text_sm().child(format!("{value} MiB"))),
            );
        }
        if let Some(r) = ram {
            let total = r.total.max(1);
            let used = total.saturating_sub(r.available);
            let gib = |v: u64| v as f64 / 1024.;
            let rail = div()
                .h(px(6.))
                .rounded_full()
                .overflow_hidden()
                .flex()
                .bg(rgb(LINE))
                .child(
                    div()
                        .h_full()
                        .w(relative(used as f32 / total as f32))
                        .bg(rgb(0x9593b8)),
                )
                .child(
                    div()
                        .h_full()
                        .w(relative(
                            u64::from(max).min(r.available) as f32 / total as f32,
                        ))
                        .bg(rgb(ACCENT)),
                );
            card = card.child(rail).child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_between()
                    .gap_1()
                    .child(kicker(&format!(
                        "系统已用 {:.1} / {:.1} GiB",
                        gib(used),
                        gib(total)
                    )))
                    .child(kicker(&format!(
                        "游戏 {:.1} GiB · 当前可用 {:.1} GiB",
                        gib(max.into()),
                        gib(r.available)
                    ))),
            );
        }
        card
    }
    pub(super) fn remove_instance_action(&self, busy: bool, cx: &Context<Self>) -> Div {
        let linked = self.instances[self.selected].external.is_some();
        let id = self.instances[self.selected].id.clone();
        if self.controls.confirm.as_ref() == Some(&id) {
            return panel(&format!(
                "将「{}」移入已删除实例？",
                self.instances[self.selected].name
            ))
            .child(kicker(if linked {
                "移除启动器中的关联，原目录保留。"
            } else {
                "存档和模组会一起保留，可在实例管理中恢复。"
            }))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("confirm-delete", "确认移除")
                            .disabled(busy)
                            .on_click(cx.listener(move |h, _, _, cx| {
                                h.move_instance(&id, false, cx);
                            })),
                    )
                    .child(Button::new("keep-instance", "取消").on_click(cx.listener(
                        |h, _, _, cx| {
                            h.controls.confirm = None;
                            cx.notify();
                        },
                    ))),
            );
        }
        div()
            .flex()
            .border_t_1()
            .border_color(rgb(LINE))
            .pt_1()
            .child(
                Button::new(
                    "delete-instance",
                    if linked {
                        "移除关联"
                    } else {
                        "删除实例"
                    },
                )
                .icon(if linked {
                    IconName::Link
                } else {
                    IconName::Trash2
                })
                .variant(ButtonVariant::Ghost)
                .disabled(busy)
                .on_click(cx.listener(move |h, _, _, cx| {
                    h.controls.confirm = Some(id.clone());
                    cx.notify();
                })),
            )
    }
    fn move_instance(&mut self, id: &str, restore: bool, cx: &mut Context<Self>) {
        let r =
            library::move_instance(&self.root, id, restore).and_then(|_| library::load(&self.root));
        match r {
            Ok(v) => {
                self.instances = v;
                self.selected = self.instances.iter().position(|i| i.id == id).unwrap_or(0);
                self.page = Page::Library;
                self.controls.confirm = None;
                self.controls.removed = None;
                self.failed = false;
                self.status = if restore {
                    "实例已恢复"
                } else {
                    "实例已移入已删除实例"
                }
                .into();
            }
            Err(e) => self.message(Err(e), cx),
        }
        cx.notify();
    }
    pub(super) fn removed_panel(&self, cx: &Context<Self>) -> Div {
        let mut row = div().flex().flex_col().gap_2().child(
            Button::new(
                "removed-instances",
                if self.controls.removed.is_some() {
                    "收起已删除实例"
                } else {
                    "已删除实例"
                },
            )
            .on_click(cx.listener(|h, _, _, cx| {
                if h.controls.removed.is_some() {
                    h.controls.removed = None;
                } else {
                    match library::removed(&h.root) {
                        Ok(v) => h.controls.removed = Some(v),
                        Err(e) => h.message(Err(e), cx),
                    }
                }
                cx.notify();
            })),
        );
        if let Some(items) = &self.controls.removed {
            if items.is_empty() {
                row = row.child(kicker("没有已删除的实例"));
            }
            for (i, p) in items.iter().enumerate() {
                let id = p.id.clone();
                row = row.child(
                    div()
                        .flex()
                        .justify_between()
                        .items_center()
                        .child(p.name.clone())
                        .child(Button::new(("restore-deleted", i), "恢复").on_click(
                            cx.listener(move |h, _, _, cx| h.move_instance(&id, true, cx)),
                        )),
                );
            }
        }
        row
    }
}
