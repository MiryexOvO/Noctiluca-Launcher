use ely_gpui_component::theme::{ActiveTheme, Mode, Palette, Theme};
use gpui::{App, Background, linear_color_stop, linear_gradient, rgb};

fn palette(choice: u8) -> Palette {
    let index = match choice {
        1 => 1,
        2 => 2,
        _ => 0,
    };
    let mut colors = Palette::dark(false);
    // Each row lists lavender, mist blue, and dusty rose in settings order.
    for (token, shades) in [
        ("bg", [0x323446, 0x1c2a40, 0x342d3b]),
        ("overlay", [0x433944, 0x405773, 0x53414e]),
        ("surface", [0x3c3b4c, 0x304055, 0x433944]),
        ("sunken", [0x302e3d, 0x253347, 0x332b36]),
        ("hover", [0x50495b, 0x425670, 0x584853]),
        ("active", [0x55475e, 0x405a7a, 0x634e5c]),
        ("border", [0x565263, 0x50657f, 0x665460]),
        ("border_strong", [0x786b82, 0x768fae, 0x937785]),
        ("fg", [0xf2edf5, 0xecf3fc, 0xf6eef1]),
        ("fg_muted", [0xc9bfd0, 0xc3d1e3, 0xd7c3cd]),
        ("fg_subtle", [0xb9adbf, 0xa9bdd5, 0xc4acb8]),
        ("accent", [0xd3b1e6, 0xb1c8eb, 0xe4b7cd]),
        ("accent_hover", [0xe1c5f0, 0xcbdff8, 0xf2cfe0]),
        ("on_accent", [0x302638, 0x192a40, 0x382833]),
        ("link", [0xbac8ff, 0xb6d9ff, 0xf6c7dd]),
    ] {
        *colors.token_mut(token) = rgb(shades[index]).into();
    }
    colors.focus = colors.accent;
    colors.selection = colors.active;
    colors.success = colors.accent;
    colors.success_subtle = colors.active;
    colors
}

pub(crate) fn apply_palette(cx: &mut App, animate: bool) {
    super::motion::sync_preference(cx);
    let immediate = !animate || cx.theme().reduced_motion;
    Theme::set_palette(
        Mode::Dark,
        Some(palette(crate::preferences::current().accent)),
        cx,
    );
    if immediate {
        Theme::set_mode_now(Mode::Dark, cx);
    }
}

pub(crate) fn window_background(cx: &App) -> Background {
    let colors = &cx.theme().colors;
    linear_gradient(
        116.,
        linear_color_stop(colors.overlay, 0.),
        linear_color_stop(colors.bg, 1.),
    )
}
