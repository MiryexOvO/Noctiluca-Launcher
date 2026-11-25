use super::{ACCENT, BG, INK, LINE, SIDEBAR};
use ely_gpui_component::{
    primitives::{Icon, IconName},
    shell::drag_region,
    theme::ActiveTheme,
};
use gpui::{prelude::*, *};

fn caption_button(
    id: &'static str,
    label: &'static str,
    area: WindowControlArea,
    tint: u32,
    missing_dot: Option<(u8, u8)>,
    activate: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let color = rgb(tint);
    let diameter = px(3.);
    let step = px(4.);
    let extent = diameter + step * 2.;
    let glyph = canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            for y in 0..3 {
                for x in 0..3 {
                    if missing_dot == Some((x, y)) {
                        continue;
                    }
                    let offset = point(step * f32::from(x), step * f32::from(y));
                    let dot = Bounds::new(bounds.origin + offset, size(diameter, diameter));
                    window.paint_quad(fill(dot, color).corner_radii(diameter * 0.5));
                }
            }
        },
    )
    .size(extent);
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .w(px(40.))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .window_control_area(area)
        .hover(move |style| style.bg(color.opacity(0.14)))
        .active(move |style| style.bg(color.opacity(0.24)))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |_, window, cx| activate(window, cx))
        .child(glyph)
}

pub(crate) fn title_bar(
    window: &mut Window,
    cx: &mut App,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let maximized = window.is_maximized();
    let drag = drag_region("titlebar-drag", window, cx);
    div()
        .flex()
        .flex_none()
        .h(cx.theme().titlebar_height())
        .bg(linear_gradient(
            90.,
            linear_color_stop(rgb(SIDEBAR), 0.),
            linear_color_stop(rgb(BG), 1.),
        ))
        .border_b_1()
        .border_color(rgb(LINE))
        .child(
            drag.flex_1()
                .h_full()
                .pl_3()
                .flex()
                .items_center()
                .gap_2()
                .child(Icon::new(IconName::Layers).color(rgb(ACCENT)))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(INK))
                        .child(crate::APP_NAME),
                ),
        )
        .child(
            div()
                .id("window-controls")
                .flex()
                .flex_none()
                .h_full()
                .child(caption_button(
                    "caption-minimize",
                    "最小化",
                    WindowControlArea::Min,
                    0xe9b772,
                    Some((0, 0)),
                    |window, _| window.minimize_window(),
                ))
                .child(caption_button(
                    "caption-maximize",
                    if maximized { "还原" } else { "最大化" },
                    WindowControlArea::Max,
                    0x98cad3,
                    Some(if maximized { (2, 0) } else { (2, 2) }),
                    |window, _| window.zoom_window(),
                ))
                .child(caption_button(
                    "caption-close",
                    "关闭",
                    WindowControlArea::Close,
                    0xe7b9b7,
                    None,
                    on_close,
                )),
        )
}
