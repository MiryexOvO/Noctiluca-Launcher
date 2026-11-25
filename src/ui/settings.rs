use super::*;

pub(super) struct Fields {
    java: Entity<TextInput>,
    jvm: Entity<TextInput>,
    resolution: Entity<TextInput>,
    cache: Entity<TextInput>,
}

impl Fields {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Home>) -> Self {
        Self {
            java: cx.new(|cx| {
                TextInput::new(window, cx)
                    .placeholder("自动检测本机 Java，缺少合适版本时下载")
                    .label("Java 可执行文件")
            }),
            jvm: cx.new(|cx| {
                TextInput::new(window, cx)
                    .multi_line(2, 5)
                    .placeholder("每行一个 JVM 参数")
                    .label("JVM 参数")
            }),
            resolution: cx.new(|cx| {
                TextInput::new(window, cx)
                    .placeholder("游戏默认尺寸，例如 1280x720")
                    .label("窗口分辨率")
            }),
            cache: cx.new(|cx| {
                TextInput::new(window, cx)
                    .placeholder("可选，例如 D:\\.minecraft")
                    .label("已有资源目录")
            }),
        }
    }
}

impl Home {
    pub(super) fn open_settings(&mut self, cx: &mut Context<Self>) {
        let item = &self.instances[self.selected];
        let path = item
            .java_path
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.fields
            .java
            .update(cx, |field, cx| field.set_text(path, cx));
        self.fields
            .jvm
            .update(cx, |field, cx| field.set_text(item.jvm_args.join("\n"), cx));
        self.fields.resolution.update(cx, |field, cx| {
            field.set_text(
                item.resolution
                    .map(|(w, h)| format!("{w}x{h}"))
                    .unwrap_or_default(),
                cx,
            )
        });
        let source = library::cache_source(&self.root);
        match source {
            Ok(source) => self.fields.cache.update(cx, |field, cx| {
                field.set_text(
                    source.map(|p| p.display().to_string()).unwrap_or_default(),
                    cx,
                )
            }),
            Err(e) => self.message(Err(e), cx),
        }
        self.page = Page::Settings;
        cx.notify();
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) -> Result<()> {
        let mut item = self.instances[self.selected].clone();
        let java = self.fields.java.read(cx).text().trim();
        item.java_path = if java.is_empty() {
            None
        } else {
            let p = PathBuf::from(java.trim_matches('"'));
            ensure!(
                p.is_absolute() && p.is_file(),
                "请选择存在的 Java 可执行文件"
            );
            Some(p)
        };
        item.jvm_args = self
            .fields
            .jvm
            .read(cx)
            .text()
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        ensure!(
            item.jvm_args.iter().all(|s| s.starts_with('-')),
            "每一行都应以 JVM 选项开头"
        );
        ensure!(
            item.jvm_args
                .iter()
                .all(|arg| !arg.starts_with("-Xmx") && !arg.starts_with("-Xms")),
            "请在游戏内存面板设置初始和最大内存"
        );
        let resolution = self
            .fields
            .resolution
            .read(cx)
            .text()
            .trim()
            .replace(['X', '×'], "x");
        item.resolution = if resolution.is_empty() {
            None
        } else {
            let (w, h) = resolution
                .split_once('x')
                .ok_or_else(|| anyhow::anyhow!("分辨率格式应为 宽x高"))?;
            let (w, h) = (w.trim().parse::<u32>()?, h.trim().parse::<u32>()?);
            ensure!(w > 0 && h > 0, "分辨率需要大于零");
            Some((w, h))
        };
        let cache = self.fields.cache.read(cx).text().trim().trim_matches('"');
        let cache = if cache.is_empty() {
            None
        } else {
            let path = PathBuf::from(cache);
            ensure!(path.is_absolute() && path.is_dir(), "已有资源目录不存在");
            Some(path)
        };
        if crate::setting("CACHE_SOURCE").is_none() {
            library::atomic_write(
                &self.root.join("cache-source.json"),
                &serde_json::to_vec(&cache)?,
            )?;
        }
        library::update(&self.root, &item)?;
        self.instances[self.selected] = item;
        self.failed = false;
        self.status = "启动设置已保存".into();
        self.page = Page::Detail;
        Ok(())
    }

    pub(super) fn settings_page(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut page = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(kicker(&self.instances[self.selected].name))
            .child(heading("启动设置"));
        for (label, field) in [
            ("Java 可执行文件", &self.fields.java),
            ("JVM 参数", &self.fields.jvm),
            ("窗口分辨率", &self.fields.resolution),
            (
                "共享资源来源 · 可复用其他启动器的 .minecraft 目录",
                &self.fields.cache,
            ),
        ] {
            page = page.child(kicker(label)).child(Input::new(field));
        }
        page.child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new("save-settings", "保存")
                        .primary()
                        .on_click(cx.listener(|home, _, _, cx| {
                            let result = home.save_settings(cx);
                            home.message(result, cx);
                        })),
                )
                .child(Button::new("cancel-settings", "返回").on_click(cx.listener(
                    |home, _, _, cx| {
                        home.page = Page::Detail;
                        cx.notify();
                    },
                ))),
        )
    }
}
