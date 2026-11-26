use super::*;
use crate::preferences::Prefs;
use ely_gpui_component::forms::Slider;

pub(super) struct Settings {
    tab: usize,
    brand: Entity<TextInput>,
    server: Entity<TextInput>,
    pub(super) minimize: bool,
    pub(super) link: Option<String>,
}
impl Settings {
    pub(super) fn new(w: &mut Window, cx: &mut Context<Home>) -> Self {
        let brand = cx.new(|cx| {
            let mut f = TextInput::new(w, cx).max_len(80).label("自定义信息");
            f.set_text(crate::preferences::current().brand, cx);
            f
        });
        let server = cx.new(|cx| {
            let mut f = TextInput::new(w, cx).max_len(260).label("服务器地址");
            f.set_text(crate::preferences::current().server, cx);
            f
        });
        Self {
            tab: 0,
            brand,
            server,
            minimize: false,
            link: None,
        }
    }
}
impl Home {
    fn save_pref(&mut self, change: impl FnOnce(&mut Prefs), cx: &mut Context<Self>) {
        let mut p = crate::preferences::current();
        let old_source = p.community;
        let old_accent = p.accent;
        change(&mut p);
        let source = p.community;
        let clip = p.clipboard;
        let accent_changed = old_accent != p.accent;
        match p.save(&self.root) {
            Ok(()) => {
                if accent_changed {
                    super::apply_palette(cx, true);
                }
                if source != old_source {
                    self.hub.source = if source == 1 {
                        crate::market::Source::CurseForge
                    } else {
                        crate::market::Source::Modrinth
                    };
                    self.hub.clear();
                    self.content.curse = source == 1;
                }
                if !clip {
                    self.prefs.link = None;
                }
                self.status = "设置已保存".into();
                self.failed = false;
            }
            Err(e) => self.message(Err(e), cx),
        }
        cx.notify();
    }
    fn choices(
        &self,
        id: &'static str,
        title: &'static str,
        labels: &[&'static str],
        value: u8,
        change: fn(&mut Prefs, u8),
        cx: &Context<Self>,
    ) -> Div {
        let mut row = div().flex().flex_wrap().gap_1();
        for (i, label) in labels.iter().enumerate() {
            row = row.child(
                Button::new((id, i), *label)
                    .when(value == i as u8, |b| b.primary())
                    .on_click(
                        cx.listener(move |h, _, _, cx| h.save_pref(|p| change(p, i as u8), cx)),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(kicker(cx, title))
            .child(row)
    }
    fn switch(
        &self,
        id: &'static str,
        label: &'static str,
        on: bool,
        change: fn(&mut Prefs, bool),
        cx: &Context<Self>,
    ) -> impl IntoElement {
        Button::new(id, format!("{}  {label}", if on { "✓" } else { "○" }))
            .when(on, |b| b.primary())
            .on_click(cx.listener(move |h, _, _, cx| h.save_pref(|p| change(p, !on), cx)))
    }
    fn scale(
        &self,
        id: &'static str,
        label: &'static str,
        value: u16,
        range: (u16, u16),
        change: fn(&mut Prefs, u16),
        cx: &Context<Self>,
    ) -> Div {
        let view = cx.weak_entity();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(kicker(cx, &format!("{label} · {value}")))
            .child(
                Slider::new(id, f64::from(value))
                    .range(range.0.into(), range.1.into())
                    .step(1.)
                    .on_change(move |v, _, cx| {
                        let _ = view.update(cx, |h, cx| h.save_pref(|p| change(p, v as u16), cx));
                    }),
            )
    }
    fn pick_java(&mut self, cx: &mut Context<Self>) {
        let pick = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 java.exe 或 javaw.exe".into()),
        });
        cx.spawn(async move |view, cx| {
            let choice = pick.await;
            let _ = view.update(cx, |h, cx| match choice {
                Ok(Ok(Some(v))) => {
                    if let Some(p) = v.first() {
                        if p.file_name().and_then(|n| n.to_str()).is_some_and(|s| {
                            s.eq_ignore_ascii_case("java.exe")
                                || s.eq_ignore_ascii_case("javaw.exe")
                        }) {
                            h.save_pref(|v| v.java = Some(p.clone()), cx);
                        } else {
                            h.message(Err(anyhow::anyhow!("请选择 Java 可执行文件")), cx)
                        }
                    }
                }
                Ok(Err(e)) => h.message(Err(e), cx),
                _ => {}
            });
        })
        .detach();
    }
    pub(super) fn activate_clipboard(&mut self, cx: &mut Context<Self>) {
        if !crate::preferences::current().clipboard {
            return;
        }
        self.prefs.link = cx
            .read_from_clipboard()
            .and_then(|c| c.text())
            .and_then(|s| crate::preferences::clipboard_link(&s))
            .map(|v| v.into());
        cx.notify();
    }
    pub(super) fn preferences_page(&self, cx: &Context<Self>) -> Div {
        let p = crate::preferences::current();
        let mut tabs = div().flex().flex_wrap().gap_1();
        for (i, title) in [
            "启动",
            "Java 与内存",
            "下载管理",
            "内容与辅助",
            "外观与语言",
            "联机",
            "关于",
        ]
        .into_iter()
        .enumerate()
        {
            tabs = tabs.child(
                Button::new(("settings-tab", i), title)
                    .when(self.prefs.tab == i, |b| b.primary())
                    .on_click(cx.listener(move |h, _, _, cx| {
                        h.prefs.tab = i;
                        cx.notify();
                    })),
            );
        }
        let page = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(heading("启动器设置"))
            .child(tabs);
        match self.prefs.tab {
            0 => page.child(
                panel(cx, "启动选项")
                    .child(self.choices(
                        "isolation",
                        "已有游戏目录的实例隔离",
                        &["自动识别", "版本隔离", "共用原目录"],
                        p.isolated,
                        |p, v| p.isolated = v,
                        cx,
                    ))
                    .child(self.choices(
                        "game-title",
                        "游戏窗口标题",
                        &["游戏默认", "实例名称", "自定义信息"],
                        p.game_title,
                        |p, v| p.game_title = v,
                        cx,
                    ))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.prefs.brand)))
                            .child(Button::new("save-brand", "保存").on_click(cx.listener(
                                |h, _, _, cx| {
                                    let s = h.prefs.brand.read(cx).text().trim().to_string();
                                    h.save_pref(|p| p.brand = s, cx);
                                },
                            ))),
                    )
                    .child(self.choices(
                        "visibility",
                        "启动器可见性",
                        &["保持窗口", "游戏启动后最小化"],
                        p.visibility,
                        |p, v| p.visibility = v,
                        cx,
                    ))
                    .child(self.choices(
                        "priority",
                        "游戏进程优先级",
                        &["较低", "正常", "较高"],
                        p.priority,
                        |p, v| p.priority = v,
                        cx,
                    ))
                    .child(self.choices(
                        "game-size",
                        "默认游戏窗口",
                        &["游戏默认", "1280 × 720", "1600 × 900", "1920 × 1080"],
                        p.size,
                        |p, v| p.size = v,
                        cx,
                    ))
                    .child(self.choices(
                        "ip",
                        "Java 网络协议偏好",
                        &["Java 默认", "IPv4", "IPv6"],
                        p.ip,
                        |p, v| p.ip = v,
                        cx,
                    ))
                    .child(kicker(cx, "Microsoft 正版验证 · 设备代码流")),
            ),
            1 => {
                let g = crate::memory::Global::read(&self.root).unwrap_or_default();
                let mut memory = panel(cx, "全局游戏内存").child(
                    div().flex().gap_2().children(
                        [(true, "自动配置"), (false, "自定义")]
                            .into_iter()
                            .map(|(auto, label)| {
                                Button::new(label, label)
                                    .when(g.automatic == auto, |b| b.primary())
                                    .on_click(cx.listener(move |h, _, _, cx| {
                                        let mut g = g;
                                        g.automatic = auto;
                                        let r = g.save(&h.root);
                                        h.controls.global = g;
                                        h.message(r, cx);
                                    }))
                            }),
                    ),
                );
                for (i, (label, v, min, max)) in [
                    ("最大内存 -Xmx", g.max, 512, 32768),
                    ("初始内存 -Xms", g.initial, 128, g.max),
                ]
                .into_iter()
                .enumerate()
                {
                    let view = cx.weak_entity();
                    memory = memory
                        .child(kicker(cx, &format!("{label} · {v} MiB")))
                        .child(
                            Slider::new(("global-heap", i), v as f64)
                                .range(min as f64, max as f64)
                                .step(128.)
                                .disabled(g.automatic)
                                .on_change(move |v, _, cx| {
                                    let _ = view.update(cx, |h, cx| {
                                        let mut g = h.controls.global;
                                        if i == 0 {
                                            g.max = v as u32;
                                            g.initial = g.initial.min(g.max);
                                        } else {
                                            g.initial = v as u32;
                                        }
                                        let r = g.save(&h.root);
                                        if r.is_ok() {
                                            h.controls.global = g;
                                        }
                                        h.message(r, cx);
                                    });
                                }),
                        );
                }
                page.child(
                    panel(cx, "Java 运行时")
                        .child(kicker(
                            cx,
                            &p.java
                                .as_ref()
                                .map(|v| v.display().to_string())
                                .unwrap_or_else(|| "自动检测本机 Java".into()),
                        ))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    Button::new("global-java", "选择 Java")
                                        .on_click(cx.listener(|h, _, _, cx| h.pick_java(cx))),
                                )
                                .child(Button::new("auto-java", "自动检测").on_click(
                                    cx.listener(|h, _, _, cx| h.save_pref(|p| p.java = None, cx)),
                                )),
                        ),
                )
                .child(memory)
            }
            2 => page.child(
                panel(cx, "游戏资源获取")
                    .child(self.choices(
                        "file-source",
                        "文件下载源",
                        &["官方优先，失败换源", "BMCLAPI 优先", "仅官方"],
                        p.files,
                        |p, v| p.files = v,
                        cx,
                    ))
                    .child(self.choices(
                        "list-source",
                        "版本列表源",
                        &["官方优先，失败换源", "BMCLAPI 优先", "仅官方"],
                        p.catalog,
                        |p, v| p.catalog = v,
                        cx,
                    ))
                    .child(self.choices(
                        "protocol",
                        "文件下载协议",
                        &["自动协商", "HTTP/1.1"],
                        p.protocol,
                        |p, v| p.protocol = v,
                        cx,
                    ))
                    .child(self.scale(
                        "connections",
                        "并发连接数",
                        p.connections.into(),
                        (1, 64),
                        |p, v| p.connections = v as u8,
                        cx,
                    ))
                    .child(self.scale(
                        "streams",
                        "单文件最大连接数",
                        p.streams.into(),
                        (1, 8),
                        |p, v| p.streams = v as u8,
                        cx,
                    ))
                    .child(self.scale(
                        "speed",
                        "速度限制 MiB/s（0 为不限速）",
                        p.limit,
                        (0, 128),
                        |p, v| p.limit = v,
                        cx,
                    ))
                    .child(kicker(
                        cx,
                        &format!("默认实例目录 · {}", self.root.join("instances").display()),
                    ))
                    .child(self.switch(
                        "select-new",
                        "安装后选中新实例",
                        p.select_new,
                        |p, v| p.select_new = v,
                        cx,
                    ))
                    .child(self.switch(
                        "authlib-update",
                        "优先使用本机已校验的同系列 Authlib 修订版",
                        p.authlib,
                        |p, v| p.authlib = v,
                        cx,
                    )),
            ),
            3 => page
                .child(
                    panel(cx, "社区资源")
                        .child(self.choices(
                            "community",
                            "首选内容平台",
                            &["Modrinth", "CurseForge"],
                            p.community,
                            |p, v| p.community = v,
                            cx,
                        ))
                        .child(self.choices(
                            "file-style",
                            "模组下载文件名",
                            &["保留原文件名", "添加项目编号"],
                            p.file_style,
                            |p, v| p.file_style = v,
                            cx,
                        ))
                        .child(self.choices(
                            "mod-style",
                            "模组列表显示",
                            &["模组名与版本", "文件名"],
                            p.name_style,
                            |p, v| p.name_style = v,
                            cx,
                        ))
                        .child(self.choices(
                            "quick",
                            "快速下载整合包",
                            &["先预览", "文件齐备时直接安装"],
                            p.quick,
                            |p, v| p.quick = v,
                            cx,
                        ))
                        .child(self.switch(
                            "hide-quilt",
                            "隐藏 Quilt 加载器",
                            p.hide_quilt,
                            |p, v| p.hide_quilt = v,
                            cx,
                        ))
                        .child(self.switch(
                            "mod-deps",
                            "安装模组时包含必需依赖",
                            p.dependencies,
                            |p, v| p.dependencies = v,
                            cx,
                        )),
                )
                .child(
                    panel(cx, "辅助功能")
                        .child(self.switch(
                            "release-news",
                            "Minecraft 正式版更新提示",
                            p.stable,
                            |p, v| p.stable = v,
                            cx,
                        ))
                        .child(self.switch(
                            "snapshot-news",
                            "Minecraft 快照更新提示",
                            p.snapshots,
                            |p, v| p.snapshots = v,
                            cx,
                        ))
                        .child(self.switch(
                            "game-lang",
                            "启动时设置游戏语言",
                            p.sync_lang,
                            |p, v| p.sync_lang = v,
                            cx,
                        ))
                        .child(self.choices(
                            "language",
                            "游戏语言",
                            &["简体中文", "English", "日本語"],
                            p.language,
                            |p, v| p.language = v,
                            cx,
                        ))
                        .child(self.switch(
                            "clipboard-links",
                            "识别剪贴板中的社区链接",
                            p.clipboard,
                            |p, v| p.clipboard = v,
                            cx,
                        )),
                ),
            4 => page.child(
                panel(cx, "外观与语言")
                    .child(self.choices(
                        "accent",
                        "界面配色",
                        &["淡紫", "雾蓝", "灰粉"],
                        p.accent,
                        |p, v| p.accent = v,
                        cx,
                    ))
                    .child(self.choices(
                        "ui-game-language",
                        "游戏语言",
                        &["简体中文", "English", "日本語"],
                        p.language,
                        |p, v| p.language = v,
                        cx,
                    ))
                    .child(self.switch(
                        "apply-game-language",
                        "启动时写入游戏语言",
                        p.sync_lang,
                        |p, v| p.sync_lang = v,
                        cx,
                    )),
            ),
            5 => {
                page.child(
                    panel(cx, "服务器直连")
                        .child(kicker(cx, "游戏启动后自动加入指定服务器"))
                        .child(Input::new(&self.prefs.server))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(Button::new("set-server", "保存并启用").on_click(
                                    cx.listener(|h, _, _, cx| {
                                        let a = h.prefs.server.read(cx).text().trim().to_owned();
                                        match crate::preferences::server(&a) {
                                            Ok(_) => h.save_pref(
                                                |p| {
                                                    p.server = a;
                                                    p.join_server = true;
                                                },
                                                cx,
                                            ),
                                            Err(e) => h.message(Err(e), cx),
                                        }
                                    }),
                                ))
                                .child(
                                    Button::new("disable-server", "关闭直连")
                                        .disabled(!p.join_server)
                                        .on_click(cx.listener(|h, _, _, cx| {
                                            h.save_pref(|p| p.join_server = false, cx)
                                        })),
                                ),
                        )
                        .child(kicker(
                            cx,
                            &format!("当前 · {}", if p.join_server { &p.server } else { "关闭" }),
                        )),
                )
            }
            _ => {
                page.child(
                    panel(cx, "软件信息")
                        .child(format!(
                            "Noctiluca {} · Rust / Ely GPUI",
                            env!("CARGO_PKG_VERSION")
                        ))
                        .child(Button::new("release-check", "检查启动器更新").on_click(
                            cx.listener(|h, _, _, cx| {
                                let task = cx
                                    .background_executor()
                                    .spawn(async { crate::preferences::check_release() });
                                cx.spawn(async move |view, cx| {
                                    let r = task.await;
                                    let _ = view.update(cx, |h, cx| match r {
                                        Ok(s) => {
                                            h.status = s;
                                            h.failed = false;
                                            cx.notify();
                                        }
                                        Err(e) => h.message(Err(e), cx),
                                    });
                                })
                                .detach();
                                h.status = "检查公开发行版".into();
                                cx.notify();
                            }),
                        ))
                        .when(!self.instances.is_empty(), |el| {
                            el.child(Button::new("settings-logs", "查看当前实例日志").on_click(
                                cx.listener(|h, _, _, cx| h.start_console("last-launch.log", cx)),
                            ))
                        })
                        .children(
                            [
                                (
                                    "release-site",
                                    "下载发行版",
                                    "https://github.com/MiryexOvO/Noctiluca-Launcher/releases",
                                ),
                                (
                                    "feedback",
                                    "问题反馈",
                                    "https://github.com/MiryexOvO/Noctiluca-Launcher/issues",
                                ),
                            ]
                            .into_iter()
                            .map(|(id, label, url)| {
                                Button::new(id, label).on_click(cx.listener(move |h, _, _, cx| {
                                    h.message(open::that_detached(url).map_err(Into::into), cx)
                                }))
                            }),
                        ),
                )
            }
        }
    }
}
