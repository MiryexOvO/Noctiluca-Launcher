#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod accounts;
mod avatar;
mod backups;
mod content;
mod curse;
mod download;
mod folders;
mod forge;
mod game;
mod java;
mod library;
mod logs;
mod market;
mod memory;
mod mods;
mod packs;
mod preferences;
mod third_party;
mod ui;

use ely_gpui_component::Assets;
use gpui::{prelude::*, *};

pub const APP_NAME: &str = "Noctiluca";

pub fn setting(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(format!("NOCTILUCA_{name}"))
        .or_else(|| std::env::var_os(format!("MCSTART_{name}")))
}

fn main() {
    gpui_platform::application()
        .with_assets(Assets)
        .run(|cx: &mut App| {
            ely_gpui_component::init(cx).expect("Ely 初始化失败");

            let bounds = Bounds::centered(None, size(px(930.), px(600.)), cx);
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(APP_NAME.into()),
                    appears_transparent: true,
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(720.), px(480.))),
                ..Default::default()
            };
            let library = library::data_root().and_then(|root| {
                preferences::Prefs::init(&root)?;
                Ok((
                    library::load(&root)?,
                    accounts::Accounts::read(&root)?,
                    root,
                ))
            });
            ui::apply_palette(cx);
            match library {
                Ok((items, accounts, root)) => {
                    cx.open_window(options, |window, cx| {
                        cx.new(|cx| ui::Home::new(root, items, accounts, window, cx))
                    })
                    .expect("无法打开 Noctiluca");
                }
                Err(error) => {
                    cx.open_window(options, |_, cx| {
                        cx.new(|_| StartupError(format!("{error:#}")))
                    })
                    .expect("无法打开错误提示");
                }
            }
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}

struct StartupError(String);
impl Render for StartupError {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(ui::BG))
            .text_color(rgb(ui::INK))
            .child(ui::title_bar(window, cx, |window, _| {
                window.remove_window()
            }))
            .child(
                div()
                    .p_8()
                    .child("无法打开 Noctiluca 实例库")
                    .child(div().mt_4().child(self.0.clone())),
            )
    }
}
