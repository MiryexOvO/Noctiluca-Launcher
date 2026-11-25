mod hub;
mod instance_controls;
mod manager;
mod motion;
mod pack_import;
mod players;
mod settings;
mod titlebar;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
pub(crate) use titlebar::title_bar;

use anyhow::{Result, ensure};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton},
    forms::{Input, TextInput},
    primitives::{FocusScope, Icon, IconName},
    theme::ActiveTheme,
};
use gpui::{prelude::*, *};

use crate::{
    accounts::{self, Accounts},
    backups, game,
    library::{self, Instance, Loader},
    mods,
};

pub(crate) const BG: u32 = 0x323446;
const PANEL: u32 = 0x3c3b4c;
const SIDEBAR: u32 = 0x413745;
const LINE: u32 = 0x565263;
pub(crate) const INK: u32 = 0xf2edf5;
const MUTED: u32 = 0xc9bfd0;
const ACCENT: u32 = 0xd3b1e6;
const SELECTED: u32 = 0x55475e;

pub fn apply_palette(cx: &mut App) {
    use ely_gpui_component::theme::{Mode, Palette, Theme};
    let mut palette = Palette::dark(false);
    for (token, color) in [
        ("bg", BG),
        ("surface", PANEL),
        ("sunken", 0x302e3d),
        ("overlay", 0x454052),
        ("hover", 0x50495b),
        ("active", SELECTED),
        ("border", LINE),
        ("border_strong", 0x786b82),
        ("fg", INK),
        ("fg_muted", MUTED),
        ("fg_subtle", 0xb9adbf),
        ("accent", ACCENT),
        ("accent_hover", 0xe1c5f0),
        ("on_accent", 0x302638),
        ("focus", ACCENT),
        ("selection", SELECTED),
        ("success", ACCENT),
        ("success_subtle", SELECTED),
        ("link", 0xbac8ff),
    ] {
        *palette.token_mut(token) = rgb(color).into();
    }
    Theme::set_palette(Mode::Dark, Some(palette), cx);
    Theme::set_mode_now(Mode::Dark, cx);
    motion::sync_preference(cx);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Library,
    Hub,
    Settings,
    Console,
    Import,
    Backups,
    Mods,
    Detail,
    Create,
    Accounts,
}
enum Update {
    Started(String, u32),
    Exited(String, Result<()>),
    Imported(Box<Instance>),
    PackPreview(Result<(PathBuf, Box<crate::packs::Preview>)>),
    PackFinished(Result<()>, Option<String>),
    Status(String),
    Finished(Result<()>, &'static str),
}

pub struct Home {
    profiles: players::Profiles,
    draft: Option<pack_import::Draft>,
    controls: instance_controls::Controls,
    navigation: motion::Navigation,
    _motion_preference: Subscription,
    manager: manager::Manager,
    hub: hub::Hub,
    fields: settings::Fields,
    focus: FocusHandle,
    root: PathBuf,
    accounts: Accounts,
    launch_after_login: Option<String>,
    login_cancel: Option<Arc<AtomicBool>>,
    logging_in: bool,
    instances: Vec<Instance>,
    selected: usize,
    page: Page,
    name: Entity<TextInput>,
    query: Entity<TextInput>,
    loader: Loader,
    mod_query: Entity<TextInput>,
    hits: Vec<mods::Hit>,
    jars: Vec<String>,
    searching: bool,
    copying: bool,
    backups: Vec<backups::Backup>,
    console_log: &'static str,
    console_text: String,
    console_epoch: u64,
    versions: Vec<game::Entry>,
    fetching: bool,
    cancel: Option<Arc<AtomicBool>>,
    running: std::collections::HashMap<String, u32>,
    outcomes: std::collections::HashMap<String, (bool, String)>,
    status: String,
    failed: bool,
    _query_subscription: Subscription,
}

impl Home {
    pub fn new(
        root: PathBuf,
        instances: Vec<Instance>,
        accounts: Accounts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let view = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            view.update(cx, Self::can_close).unwrap_or(true)
        });
        let name = cx.new(|cx| {
            TextInput::new(window, cx)
                .label("实例名称")
                .placeholder("例如：周末生存")
                .max_len(80)
        });
        let query = cx.new(|cx| {
            TextInput::new(window, cx)
                .label("游戏版本")
                .placeholder("输入版本号，例如 1.20.1")
                .max_len(80)
        });
        let subscription = cx.observe(&query, |_, _, cx| cx.notify());
        let mod_query = cx.new(|cx| {
            TextInput::new(window, cx)
                .label("搜索 Modrinth")
                .placeholder("输入模组名称")
                .max_len(100)
        });
        let initial_page = if instances.is_empty() {
            Page::Library
        } else {
            Page::Detail
        };
        let mut home = Self {
            profiles: players::Profiles::new(window, cx),
            draft: None,
            controls: instance_controls::Controls::new(&root),
            navigation: Default::default(),
            _motion_preference: cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    motion::sync_preference(cx);
                }
            }),
            launch_after_login: None,
            login_cancel: None,
            manager: manager::Manager::new(&root, window, cx),
            hub: hub::Hub::new(window, cx),
            fields: settings::Fields::new(window, cx),
            focus,
            root,
            instances,
            accounts,
            logging_in: false,
            selected: 0,
            page: initial_page,
            name,
            query,
            loader: Loader::Vanilla,
            mod_query,
            hits: Vec::new(),
            jars: Vec::new(),
            searching: false,
            copying: false,
            backups: Vec::new(),
            console_log: "last-launch.log",
            console_text: String::new(),
            console_epoch: 0,
            versions: vec![],
            fetching: false,
            cancel: None,
            running: Default::default(),
            outcomes: Default::default(),
            status: "就绪".into(),
            failed: false,
            _query_subscription: subscription,
        };
        let args: Vec<_> = std::env::args().skip(1).collect();
        if let [flag, id] = args.as_slice()
            && flag == "--launch"
        {
            if let Some(index) = home.instances.iter().position(|i| &i.id == id) {
                home.selected = index;
                home.play(false, cx);
            } else {
                home.failed = true;
                home.status = format!("找不到实例 {id}");
            }
        }
        home.refresh_avatar(cx);
        home
    }

    fn can_close(&mut self, cx: &mut Context<Self>) -> bool {
        let reason = if self.copying {
            "正在复制实例，请等待文件写入完成"
        } else if !self.running.is_empty() {
            "请先退出游戏，或将启动器最小化"
        } else if self.cancel.is_some() {
            "请先取消安装，再关闭窗口"
        } else {
            return true;
        };
        self.status = reason.into();
        cx.notify();
        false
    }

    fn message(&mut self, result: Result<()>, cx: &mut Context<Self>) {
        if let Err(error) = result {
            self.failed = true;
            self.status = format!("{error:#}");
        }
        cx.notify();
    }

    fn play(&mut self, install_only: bool, cx: &mut Context<Self>) {
        if self.instances.get(self.selected).is_none()
            || self.cancel.is_some()
            || self.logging_in
            || self.running.contains_key(&self.instances[self.selected].id)
        {
            return;
        }
        if !install_only && self.accounts.current().is_none() {
            self.open_accounts(cx);
            self.launch_after_login = Some(self.instances[self.selected].id.clone());
            self.status = "请选择账号，登录后将启动此实例".into();
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.failed = false;
        self.status = "正在准备启动".into();
        let root = self.root.clone();
        let instance = self.instances[self.selected].clone();
        self.outcomes.remove(&instance.id);
        let player = self.accounts.current().cloned();
        let (tx, rx) = async_channel::unbounded();
        std::thread::spawn(move || {
            let result = game::play(
                &root,
                &instance,
                player.as_ref(),
                install_only,
                &cancel,
                |s| {
                    let update = match s
                        .strip_prefix("游戏已启动 · PID ")
                        .and_then(|pid| pid.parse::<u32>().ok())
                    {
                        Some(pid) => Update::Started(instance.id.clone(), pid),
                        None => Update::Status(s),
                    };
                    let _ = tx.try_send(update);
                },
            );
            let _ = tx.try_send(if install_only {
                Update::Finished(result, "安装与校验完成")
            } else {
                Update::Exited(instance.id, result)
            });
        });
        self.listen(rx, cx);
    }

    fn listen(&mut self, rx: async_channel::Receiver<Update>, cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| {
            while let Ok(update) = rx.recv().await {
                if view
                    .update(cx, |home, cx| {
                        match update {
                            Update::Started(id, pid) => {
                                home.running.insert(id, pid);
                                home.cancel = None;
                                home.status = format!("游戏运行中 · PID {pid}");
                            }
                            Update::Exited(id, result) => {
                                if home.running.remove(&id).is_none() {
                                    home.cancel = None;
                                }
                                let (failed, msg) = match result {
                                    Ok(()) => (false, "游戏已正常退出".to_string()),
                                    Err(e) => (true, format!("{e:#}")),
                                };
                                if home
                                    .instances
                                    .get(home.selected)
                                    .is_some_and(|i| i.id == id)
                                    && matches!(home.page, Page::Detail | Page::Console)
                                    && home.cancel.is_none()
                                {
                                    home.status = msg.clone();
                                    home.failed = failed;
                                }
                                home.outcomes.insert(id, (failed, msg));
                            }
                            Update::PackPreview(result) => home.preview_ready(result, cx),
                            Update::PackFinished(result, launch) => {
                                home.pack_finished(result, launch, cx)
                            }
                            Update::Imported(item) => {
                                home.draft = None;
                                home.instances.push(*item);
                                home.selected = home.instances.len() - 1;
                                home.page = Page::Detail;
                            }
                            Update::Status(text) => {
                                home.status = text;
                            }
                            Update::Finished(result, success) => {
                                home.cancel = None;
                                home.status = success.into();
                                if matches!(home.page, Page::Mods) {
                                    home.refresh_mods(cx);
                                }
                                home.message(result, cx);
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn fetch_versions(&mut self, refresh: bool, cx: &mut Context<Self>) {
        if self.fetching || (!refresh && !self.versions.is_empty()) {
            return;
        }
        self.fetching = true;
        let root = self.root.clone();
        let task = cx
            .background_executor()
            .spawn(async move { game::catalog(&root, refresh) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |home, cx| {
                home.fetching = false;
                match result {
                    Ok(versions) => home.versions = versions,
                    Err(e) => home.message(Err(e), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn create(&mut self, cx: &mut Context<Self>) -> Result<()> {
        let version = self.query.read(cx).text().trim().to_owned();
        if !self.versions.is_empty() {
            ensure!(
                self.versions.iter().any(|v| v.id == version),
                "请从官方列表选择有效的游戏版本"
            );
        }
        let item = Instance::new(self.name.read(cx).text().into(), version, self.loader)?;
        library::save(&self.root, &item)?;
        self.instances.push(item);
        self.selected = self.instances.len() - 1;
        self.page = Page::Detail;
        self.status = "实例已创建".into();
        self.failed = false;
        Ok(())
    }

    fn open(&mut self, relative: &str, cx: &mut Context<Self>) {
        let item = &self.instances[self.selected];
        let result = (|| -> Result<()> {
            let path = if let Some(subdir) = relative.strip_prefix("game/") {
                item.game_dir(&self.root)?.join(subdir)
            } else {
                item.path(&self.root).join(relative)
            };
            if relative.ends_with(".log") {
                ensure!(path.is_file(), "这次启动尚未生成该日志");
                Command::new("notepad.exe").arg(path).spawn()?;
            } else {
                fs::create_dir_all(&path)?;
                Command::new("explorer.exe").arg(path).spawn()?;
            }
            Ok(())
        })();
        self.message(result, cx)
    }

    fn sidebar(&self, frame: motion::Frame, cx: &Context<Self>) -> impl IntoElement {
        let busy = self.cancel.is_some() || self.searching || self.logging_in || self.copying;
        let mut list = div()
            .relative()
            .flex()
            .flex_col()
            .gap(px(motion::GAP))
            .when(matches!(self.page, Page::Detail), |list| {
                list.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(frame.row * (motion::ROW + motion::GAP)))
                        .h(px(motion::ROW))
                        .rounded_md()
                        .bg(linear_gradient(
                            90.,
                            linear_color_stop(rgb(SELECTED), 0.),
                            linear_color_stop(rgb(0x4b4055), 1.),
                        ))
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top(px(17.))
                                .w(px(3.))
                                .h(px(26.))
                                .rounded_full()
                                .bg(rgb(ACCENT)),
                        ),
                )
            });
        for (idx, item) in self.instances.iter().enumerate() {
            let selected = idx == self.selected && matches!(self.page, Page::Detail);
            list = list.child(
                div()
                    .id(("instance", idx))
                    .h(px(motion::ROW))
                    .flex_none()
                    .rounded_md()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_3()
                    .when(!selected, |el| el.hover(|s| s.bg(rgba(0xd3b1e617))))
                    .cursor_pointer()
                    .child(Icon::new(IconName::Box).color(rgb(if selected {
                        ACCENT
                    } else {
                        MUTED
                    })))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().child(item.name.clone()))
                            .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                                "{} · {}{}",
                                item.version,
                                item.loader.label(),
                                if self.running.contains_key(&item.id) {
                                    " · 运行中"
                                } else {
                                    ""
                                }
                            ))),
                    )
                    .on_click(cx.listener(move |home, _, _, cx| {
                        if !busy {
                            home.show_instance(idx, cx);
                        }
                    })),
            );
        }
        div()
            .w(px(232.))
            .flex_none()
            .h_full()
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child(format!("实例库  /  {}", self.instances.len())),
                    )
                    .child(
                        IconButton::new("new", IconName::Plus)
                            .tooltip("新建实例")
                            .disabled(busy)
                            .on_click(cx.listener(|home, _, _, cx| {
                                home.page = Page::Create;
                                home.status = "选择游戏版本与加载器".into();
                                home.failed = false;
                                home.fetch_versions(false, cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                Button::new(
                    "sidebar-import",
                    if self.draft.is_some() {
                        "继续导入整合包"
                    } else {
                        "导入整合包"
                    },
                )
                .icon(IconName::Package)
                .full_width()
                .when(self.page == Page::Import, |b| {
                    b.variant(ButtonVariant::Subtle)
                })
                .disabled(busy)
                .on_click(cx.listener(|home, _, _, cx| home.import_pack(cx))),
            )
            .child(
                div()
                    .id("library-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list),
            )
            .child(
                Button::new("instance-manager", "实例管理")
                    .full_width()
                    .disabled(busy)
                    .when(matches!(self.page, Page::Library), |b| b.primary())
                    .on_click(cx.listener(|home, _, _, cx| {
                        home.page = Page::Library;
                        home.failed = false;
                        home.status = "就绪".into();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("downloads", "下载中心")
                    .full_width()
                    .when(matches!(self.page, Page::Hub), |b| b.primary())
                    .disabled(busy)
                    .on_click(cx.listener(|home, _, _, cx| {
                        home.page = Page::Hub;
                        home.failed = false;
                        home.status = "就绪".into();
                        home.fetch_versions(false, cx);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .pt_2()
                    .child(self.account_card(busy, cx)),
            )
    }

    fn detail(&self, frame: motion::Frame, cx: &Context<Self>) -> impl IntoElement {
        let item = &self.instances[self.selected];
        let running = self.running.contains_key(&item.id);
        let busy = self.cancel.is_some() || running;
        let mut actions = div().flex().gap_2().items_center().child(
            Button::new(
                "play",
                if running {
                    "游戏运行中"
                } else if busy {
                    "正在准备"
                } else if self.accounts.current().is_some() {
                    "启动游戏"
                } else {
                    "选择账号"
                },
            )
            .primary()
            .icon(IconName::Play)
            .disabled(busy)
            .on_click(cx.listener(|home, _, _, cx| home.play(false, cx))),
        );
        if !busy {
            actions = actions.child(
                Button::new("repair", "安装 / 修复")
                    .on_click(cx.listener(|home, _, _, cx| home.play(true, cx))),
            );
        }
        if busy && !running {
            actions = actions.child(Button::new("cancel", "取消安装").on_click(cx.listener(
                |home, _, _, cx| {
                    if let Some(cancel) = &home.cancel {
                        cancel.store(true, Ordering::Relaxed)
                    }
                    home.status = "正在取消，等待当前文件结束".into();
                    cx.notify();
                },
            )));
        }
        let mut files = div().flex().flex_wrap().gap_2();
        for (id, label, path) in [
            ("worlds", "存档", "game/saves"),
            ("mods", "模组", "game/mods"),
            ("packs", "资源包", "game/resourcepacks"),
            ("shaders", "光影", "game/shaderpacks"),
            ("screenshots", "截图", "game/screenshots"),
            ("schematics", "投影", "game/schematics"),
        ] {
            files = files.child(
                Button::new(id, label)
                    .on_click(cx.listener(move |home, _, _, cx| home.open(path, cx))),
            );
        }
        files = files.child(Button::new("console", "实时控制台").on_click(cx.listener(
            |home, _, _, cx| {
                home.start_console("last-launch.log", cx);
            },
        )));
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                frame.section(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(kicker(&format!(
                            "Minecraft Java 版  /  {}",
                            self.accounts
                                .current()
                                .map(|p| p.name.as_str())
                                .unwrap_or("未选择账号")
                        )))
                        .child(heading(&item.name))
                        .child(div().text_sm().text_color(rgb(MUTED)).child(format!(
                            "游戏版本 {}     /     {}     /     {}",
                            item.version,
                            format_args!(
                                "{} {}",
                                item.loader.label(),
                                item.loader_version.as_deref().unwrap_or("")
                            ), item.external.as_ref().map_or("独立游戏目录", |_| "原目录启动")
                        ))),
                    0,
                ),
            )
            .child(frame.section(actions, 1))
            .child(frame.section(self.memory_panel(busy, cx), 2))
            .child(
                frame.section(
                    panel("实例文件")
                        .child(files)
                        .child(self.instance_actions(busy, cx))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .child(
                                    Button::new("open-backups", "备份与克隆")
                                        .disabled(busy)
                                        .on_click(cx.listener(|home, _, _, cx| {
                                            home.page = Page::Backups;
                                            home.read_backups(cx);
                                        })),
                                )
                                .when(item.loader != Loader::Vanilla, |el| {
                                    el.child(
                                        Button::new("manage-mods", "管理模组")
                                            .disabled(busy)
                                            .on_click(cx.listener(|home, _, _, cx| {
                                                home.page = Page::Mods;
                                                home.hits.clear();
                                                home.refresh_mods(cx);
                                            })),
                                    )
                                }),
                        ),
                    3,
                ),
            )
    }

    fn start_console(&mut self, log: &'static str, cx: &mut Context<Self>) {
        self.page = Page::Console;
        self.console_log = log;
        self.console_epoch += 1;
        let epoch = self.console_epoch;
        let path = self.instances[self.selected].path(&self.root).join(log);
        self.console_text.clear();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |view, cx| {
            loop {
                let path = path.clone();
                let text = executor
                    .spawn(async move {
                        if !path.is_file() {
                            return "等待日志生成…".to_owned();
                        }
                        crate::logs::tail(&path)
                            .map(|s| crate::logs::redact(&s))
                            .unwrap_or_else(|e| format!("读取日志失败：{e}"))
                    })
                    .await;
                let active = view
                    .update(cx, |home, cx| {
                        if home.console_epoch != epoch || !matches!(home.page, Page::Console) {
                            return false;
                        }
                        if text != home.console_text {
                            home.console_text = text;
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !active {
                    break;
                }
                executor.timer(std::time::Duration::from_secs(1)).await;
            }
        })
        .detach();
        cx.notify();
    }

    fn console_page(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut tabs = div().flex().flex_wrap().gap_2();
        for (key, title) in [
            ("last-launch.log", "游戏"),
            ("launcher.log", "启动器"),
            ("loader-install.log", "加载器"),
        ] {
            tabs = tabs.child(
                Button::new(key, title)
                    .when(self.console_log == key, |b| b.primary())
                    .on_click(cx.listener(move |home, _, _, cx| home.start_console(key, cx))),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(kicker(&self.instances[self.selected].name))
            .child(heading("实时控制台"))
            .child(tabs)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(Button::new("back", "返回实例").on_click(cx.listener(
                        |home, _, _, cx| {
                            home.page = Page::Detail;
                            cx.notify();
                        },
                    )))
                    .child(
                        Button::new("full-log", "打开完整日志").on_click(
                            cx.listener(|home, _, _, cx| home.open(home.console_log, cx)),
                        ),
                    )
                    .child(
                        Button::new("copy-log", "复制脱敏日志").on_click(cx.listener(
                            |home, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    home.console_text.clone(),
                                ));
                                home.status = "最近 100 行日志已复制".into();
                                cx.notify();
                            },
                        )),
                    ),
            )
            .child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(PANEL))
                    .text_sm()
                    .font_family("Consolas")
                    .child(self.console_text.clone()),
            )
    }

    fn read_backups(&mut self, cx: &mut Context<Self>) {
        match backups::list(&self.root, &self.instances[self.selected]) {
            Ok(list) => self.backups = list,
            Err(err) => self.message(Err(err), cx),
        }
        cx.notify();
    }

    fn copy_instance(&mut self, clone: bool, from: Option<PathBuf>, cx: &mut Context<Self>) {
        self.copying = true;
        self.failed = false;
        self.status = "正在复制游戏文件".into();
        let root = self.root.clone();
        let item = self.instances[self.selected].clone();
        let task = cx.background_executor().spawn(async move {
            if clone {
                backups::duplicate(&root, &item, from.as_deref()).map(Some)
            } else {
                backups::create(&root, &item).map(|()| None)
            }
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |home, cx| {
                home.copying = false;
                match result {
                    Ok(Some(item)) => {
                        home.instances.push(item);
                        home.selected = home.instances.len() - 1;
                        home.page = Page::Detail;
                        home.status = "已创建独立实例".into();
                    }
                    Ok(None) => {
                        home.status = "备份已保存".into();
                        home.read_backups(cx);
                    }
                    Err(err) => home.message(Err(err), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn backups_page(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut page = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(kicker(&self.instances[self.selected].name))
            .child(heading("备份与克隆"))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("backup", "创建备份")
                            .primary()
                            .disabled(self.copying)
                            .on_click(
                                cx.listener(|home, _, _, cx| home.copy_instance(false, None, cx)),
                            ),
                    )
                    .child(
                        Button::new("clone", "克隆当前实例")
                            .disabled(self.copying)
                            .on_click(
                                cx.listener(|home, _, _, cx| home.copy_instance(true, None, cx)),
                            ),
                    ),
            )
            .child(kicker(&format!("本地备份  /  {}", self.backups.len())));
        for (idx, backup) in self.backups.iter().enumerate() {
            let time: chrono::DateTime<chrono::Local> = backup.created.into();
            let path = backup.path.clone();
            page = page.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(time.format("%Y-%m-%d  %H:%M:%S").to_string())
                    .child(
                        Button::new(("restore", idx), "恢复为新实例")
                            .disabled(self.copying)
                            .on_click(cx.listener(move |home, _, _, cx| {
                                home.copy_instance(true, Some(path.clone()), cx)
                            })),
                    ),
            );
        }
        page
    }

    fn refresh_mods(&mut self, cx: &mut Context<Self>) {
        let result = self.instances[self.selected]
            .game_dir(&self.root)
            .and_then(|dir| mods::installed(&dir.join("mods")));
        match result {
            Ok(jars) => self.jars = jars,
            Err(e) => self.message(Err(e), cx),
        }
        cx.notify();
    }

    fn search_mods(&mut self, cx: &mut Context<Self>) {
        self.searching = true;
        self.hits.clear();
        self.failed = false;
        self.status = "正在搜索 Modrinth".into();
        let item = self.instances[self.selected].clone();
        let query = self.mod_query.read(cx).text().to_owned();
        let task = cx
            .background_executor()
            .spawn(async move { mods::search(&item, &query) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |home, cx| {
                home.searching = false;
                match result {
                    Ok(hits) => {
                        home.hits = hits;
                        home.status = format!("找到 {} 个匹配的模组", home.hits.len());
                    }
                    Err(e) => home.message(Err(e), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn install_mod(&mut self, hit: mods::Hit, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let item = self.instances[self.selected].clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.failed = false;
        let (tx, rx) = async_channel::unbounded();
        std::thread::spawn(move || {
            let result = mods::install(&root, &item, &hit, &cancel, |s| {
                let _ = tx.try_send(Update::Status(s));
            });
            let _ = tx.try_send(Update::Finished(result, "模组与必需依赖已安装"));
        });
        self.listen(rx, cx);
    }

    fn mods_page(&self, cx: &Context<Self>) -> impl IntoElement {
        let item = &self.instances[self.selected];
        let busy = self.searching || self.cancel.is_some();
        let mut page = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(kicker(&item.name))
            .child(heading("模组管理"))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.mod_query)))
                    .child(
                        Button::new("mod-search", "搜索 Modrinth")
                            .primary()
                            .disabled(busy)
                            .on_click(cx.listener(|home, _, _, cx| home.search_mods(cx))),
                    ),
            );
        if self.cancel.is_some() {
            page = page.child(Button::new("cancel-mod", "取消安装").on_click(cx.listener(
                |home, _, _, cx| {
                    if let Some(cancel) = &home.cancel {
                        cancel.store(true, Ordering::Relaxed);
                    }
                    cx.notify();
                },
            )));
        }
        for (idx, hit) in self.hits.iter().enumerate() {
            let hit = hit.clone();
            page =
                page.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(hit.title.clone())
                        .child(
                            Button::new(("install-mod", idx), "安装")
                                .disabled(busy)
                                .on_click(cx.listener(move |home, _, _, cx| {
                                    home.install_mod(hit.clone(), cx)
                                })),
                        ),
                );
        }
        page = page.child(kicker(&format!("已安装  /  {}", self.jars.len())));
        for (idx, name) in self.jars.iter().enumerate() {
            let name = name.clone();
            let disabled = name.ends_with(".disabled");
            page = page.child(
                div()
                    .flex()
                    .gap_3()
                    .items_center()
                    .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                    .child(
                        Button::new(("toggle-mod", idx), if disabled { "启用" } else { "停用" })
                            .disabled(busy)
                            .on_click(cx.listener(move |home, _, _, cx| {
                                let result =
                                    mods::toggle(&home.root, &home.instances[home.selected], &name);
                                home.message(result, cx);
                                home.refresh_mods(cx);
                            })),
                    ),
            );
        }
        page
    }

    fn import_pack(&mut self, cx: &mut Context<Self>) {
        if self.draft.is_some() {
            self.page = Page::Import;
            cx.notify();
            return;
        }
        self.pick_pack(cx);
    }
    fn pick_pack(&mut self, cx: &mut Context<Self>) {
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 Modrinth (.mrpack) 或 CurseForge (.zip) 整合包".into()),
        });
        cx.spawn(async move |view, cx| {
            let choice = picker.await;
            let _ = view.update(cx, |home, cx| {
                let path = match choice {
                    Ok(Ok(Some(paths))) => paths.into_iter().next(),
                    Ok(Err(err)) => {
                        home.message(Err(err), cx);
                        None
                    }
                    _ => None,
                };
                let Some(path) = path else { return };
                if home.cancel.is_some() || home.copying || home.logging_in {
                    return;
                }
                home.failed = false;
                home.page = Page::Import;
                home.status = "读取整合包".into();
                home.install_pack(hub::Pack::Local(path), cx);
            });
        })
        .detach();
    }

    fn form(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut loaders = div().flex().gap_2();
        for (idx, loader) in [
            Loader::Vanilla,
            Loader::Fabric,
            Loader::Quilt,
            Loader::Forge,
            Loader::NeoForge,
        ]
        .into_iter()
        .enumerate()
        {
            loaders = loaders.child(
                Button::new(("loader", idx), loader.label())
                    .when(loader == self.loader, |b| b.primary())
                    .on_click(cx.listener(move |home, _, _, cx| {
                        home.loader = loader;
                        cx.notify();
                    })),
            );
        }
        let filter = self.query.read(cx).text().trim().to_ascii_lowercase();
        let mut matches = div().flex().flex_wrap().gap_1();
        for (idx, entry) in self
            .versions
            .iter()
            .filter(|v| {
                if filter.is_empty() {
                    v.kind == "release"
                } else {
                    v.id.to_ascii_lowercase().contains(&filter)
                }
            })
            .take(6)
            .enumerate()
        {
            let value = entry.id.clone();
            matches = matches.child(Button::new(("version", idx), value.clone()).on_click(
                cx.listener(move |home, _, _, cx| {
                    home.query
                        .update(cx, |field, cx| field.set_text(value.clone(), cx));
                }),
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(heading("新建实例"))
                    .child(
                        Button::new("import-pack", "导入整合包")
                            .on_click(cx.listener(|home, _, _, cx| home.import_pack(cx))),
                    ),
            )
            .child(kicker("实例名称"))
            .child(Input::new(&self.name))
            .child(kicker("游戏版本"))
            .child(Input::new(&self.query))
            .child(matches)
            .child(kicker("加载器"))
            .child(loaders)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("create", "创建实例")
                            .primary()
                            .on_click(cx.listener(|home, _, _, cx| {
                                let result = home.create(cx);
                                home.message(result, cx);
                            })),
                    )
                    .child(
                        Button::new(
                            "refresh",
                            if self.fetching {
                                "正在获取版本"
                            } else {
                                "刷新官方版本"
                            },
                        )
                        .disabled(self.fetching)
                        .on_click(cx.listener(|home, _, _, cx| {
                            home.fetch_versions(true, cx);
                            cx.notify();
                        })),
                    ),
            )
    }
}

fn kicker(text: &str) -> Div {
    div()
        .text_sm()
        .text_color(rgb(MUTED))
        .child(text.to_owned())
}
fn heading(text: &str) -> Div {
    div()
        .text_size(px(30.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.to_owned())
}
fn panel(title: &str) -> Div {
    div()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(PANEL))
        .flex()
        .flex_col()
        .gap_3()
        .child(kicker(title))
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Some(cancel) = &self.login_cancel {
            cancel.store(true, Ordering::Release);
        }
    }
}

impl Render for Home {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.page != Page::Accounts {
            self.launch_after_login = None;
        }
        let frame = self.navigation.frame(
            motion::Destination {
                page: self.page,
                instance: self.selected,
                packs: matches!(self.page, Page::Hub) && self.hub.packs,
            },
            cx.theme().reduced_motion,
            std::time::Instant::now(),
        );
        if frame.active() {
            window.request_animation_frame();
        }
        let page = match self.page {
            Page::Library => self.manager_page(cx).into_any_element(),
            Page::Hub => self.hub_page(cx).into_any_element(),
            Page::Detail => self.detail(frame, cx).into_any_element(),
            Page::Create => self.form(cx).into_any_element(),
            Page::Accounts => self.account_page(cx).into_any_element(),
            Page::Mods => self.mods_page(cx).into_any_element(),
            Page::Console => self.console_page(cx).into_any_element(),
            Page::Settings => self.settings_page(cx).into_any_element(),
            Page::Import => self.pack_page(frame, cx).into_any_element(),
            Page::Backups => self.backups_page(cx).into_any_element(),
        };
        let view = cx.weak_entity();
        FocusScope::new(&self.focus)
            .root()
            .size_full()
            .flex()
            .flex_col()
            .bg(linear_gradient(
                104.,
                linear_color_stop(rgb(0x433944), 0.),
                linear_color_stop(rgb(BG), 1.),
            ))
            .text_color(rgb(INK))
            .font_family("Microsoft YaHei UI")
            .child(title_bar(window, cx, move |window, cx| {
                if view.update(cx, Self::can_close).unwrap_or(true) {
                    window.remove_window();
                }
            }))
            .child(
                div()
                    .id("pack-drop")
                    .on_drop(cx.listener(|home, paths: &ExternalPaths, _, cx| {
                        home.drop_pack(paths.paths(), cx)
                    }))
                    .drag_over::<ExternalPaths>(|style, _, _, _| {
                        style.border_1().border_color(rgb(ACCENT))
                    })
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.sidebar(frame, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .id("main-scroll")
                                    .flex_1()
                                    .min_h_0()
                                    .when(self.page != Page::Import, |el| {
                                        el.overflow_y_scroll().p_6()
                                    })
                                    .child(if matches!(self.page, Page::Detail | Page::Import) {
                                        page
                                    } else {
                                        frame.section(div().child(page), 0).into_any_element()
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .border_t_1()
                                    .border_color(rgb(LINE))
                                    .px_6()
                                    .py_3()
                                    .text_sm()
                                    .text_color(rgb(if self.failed { 0xffb5c5 } else { ACCENT }))
                                    .when(
                                        self.cancel.is_some()
                                            || self.copying
                                            || self.logging_in
                                            || self.searching
                                            || self.hub.loading,
                                        |el| el.child(motion::task_progress(&self.status)),
                                    )
                                    .child(self.status.clone()),
                            ),
                    ),
            )
            .when_some(self.device_dialog(cx), |root, dialog| root.child(dialog))
    }
}
