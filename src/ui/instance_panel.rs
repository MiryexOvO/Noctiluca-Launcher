use super::*;
use ely_gpui_component::{primitives::FocusRing, theme::IconSize};

impl Home {
    pub(super) fn instance_panel(&self, wide: bool, busy: bool, cx: &Context<Self>) -> Div {
        let accent = cx.theme().colors.accent;
        let mut tiles = div()
            .grid()
            .grid_cols(3)
            .gap_2()
            .when(wide, |el| el.flex_1());
        for (id, label, icon, kind) in [
            ("worlds", "存档", IconName::Map, "saves"),
            ("mods", "模组", IconName::Puzzle, "mods"),
            ("packs", "资源包", IconName::Layers, "resourcepacks"),
            ("shaders", "光影", IconName::Sun, "shaderpacks"),
            ("screenshots", "截图", IconName::Camera, "screenshots"),
            ("schematics", "投影", IconName::Box, "schematics"),
        ] {
            tiles = tiles.child(
                div()
                    .id(id)
                    .role(Role::Button)
                    .aria_label(label)
                    .min_w_0()
                    .min_h(px(72.))
                    .rounded_md()
                    .border_1()
                    .border_color(accent.opacity(0.14))
                    .bg(accent.opacity(0.055))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .tab_index(0)
                    .focus_ring(cx)
                    .cursor_pointer()
                    .hover(|s| s.bg(accent.opacity(0.13)).border_color(accent.opacity(0.5)))
                    .active(|s| s.bg(accent.opacity(0.22)))
                    .child(Icon::new(icon).size(IconSize::Lg).color(accent))
                    .child(div().text_sm().child(label))
                    .on_click(cx.listener(move |h, _, _, cx| h.browse_files(label, kind, cx))),
            );
        }
        let mut tools = div().flex().flex_col();
        let item = &self.instances[self.selected];
        for (id, label, icon, enabled, action) in [
            (
                "content-tools",
                "组件与整合包",
                IconName::Package,
                !busy,
                Home::open_content as fn(&mut Home, &mut Context<Home>),
            ),
            (
                "manage-mods",
                "管理模组",
                IconName::Puzzle,
                !busy,
                |h, cx| {
                    h.page = Page::Mods;
                    h.hits.clear();
                    h.content.curse = crate::preferences::current().community == 1;
                    h.refresh_mods(cx);
                },
            ),
            (
                "settings",
                "启动设置",
                IconName::SlidersHorizontal,
                !busy,
                Home::open_settings,
            ),
            (
                "open-backups",
                "备份与克隆",
                IconName::History,
                !busy,
                |h, cx| {
                    h.page = Page::Backups;
                    h.read_backups(cx);
                },
            ),
            (
                "console",
                "实时控制台",
                IconName::Terminal,
                true,
                |h, cx| h.start_console("last-launch.log", cx),
            ),
        ] {
            if id == "manage-mods" && item.loader == Loader::Vanilla {
                continue;
            }
            let row = div()
                .id(id)
                .role(Role::Button)
                .aria_label(label)
                .min_w_0()
                .h(px(32.))
                .rounded_md()
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .child(Icon::new(icon).size(IconSize::Sm).color(accent))
                .child(div().flex_1().text_sm().child(label))
                .child(
                    Icon::new(IconName::ChevronRight)
                        .size(IconSize::Xs)
                        .color(rgb(MUTED)),
                );
            tools = tools.child(if enabled {
                row.tab_index(0)
                    .focus_ring(cx)
                    .cursor_pointer()
                    .hover(|s| s.bg(accent.opacity(0.10)))
                    .active(|s| s.bg(accent.opacity(0.18)))
                    .on_click(cx.listener(move |h, _, _, cx| action(h, cx)))
            } else {
                row.opacity(0.45)
                    .cursor_not_allowed()
                    .aria_description("实例正在使用")
            });
        }
        div()
            .flex()
            .gap_3()
            .when(!wide, |el| el.flex_col())
            .child(
                panel("实例资源")
                    .min_w_0()
                    .when(wide, |el| el.flex_1())
                    .child(tiles),
            )
            .child(
                panel("实例工具")
                    .min_w_0()
                    .when(wide, |el| el.w(px(218.)).flex_none())
                    .child(tools)
                    .child(self.remove_instance_action(busy, cx)),
            )
    }
}
