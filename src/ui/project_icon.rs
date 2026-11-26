use super::*;
use crate::market::{self, Kind};
use std::path::Path;

struct IconFile;
impl Asset for IconFile {
    type Source = (PathBuf, String);
    type Output = Result<PathBuf, ImageCacheError>;

    fn load(
        source: Self::Source,
        _: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let (root, url) = source;
        async move { market::icon::cached(&root, &url).map_err(Into::into) }
    }
}

pub(super) fn prepare(root: &Path, address: &str, cx: &mut App) {
    let source = (root.to_owned(), address.to_owned());
    if matches!(cx.fetch_asset::<IconFile>(&source), Some(Err(_))) {
        cx.remove_asset::<IconFile>(&source);
    }
}

pub(super) fn tile(cx: &App, root: &Path, hit: &market::Hit, kind: Kind, size: f32) -> Div {
    let icon = match kind {
        Kind::Mod => IconName::Puzzle,
        Kind::Pack => IconName::Package,
        Kind::Resource => IconName::Layers,
        Kind::Shader => IconName::Sun,
    };
    let accent = cx.theme().colors.accent;
    let placeholder = move || {
        div()
            .size_full()
            .flex()
            .justify_center()
            .items_center()
            .child(
                Icon::new(icon)
                    .size(ely_gpui_component::theme::IconSize::Lg)
                    .color(accent),
            )
            .into_any_element()
    };
    let tile = div()
        .size(px(size))
        .flex_none()
        .rounded_md()
        .overflow_hidden()
        .bg(cx.theme().colors.active);
    let Some(url) = hit.icon_url.as_deref().filter(|u| !u.trim().is_empty()) else {
        return tile.child(placeholder());
    };
    let source = (root.to_owned(), url.to_owned());
    let image = img(move |window: &mut Window, cx: &mut App| {
        match window.use_asset::<IconFile>(&source, cx)? {
            Ok(path) => window.use_asset::<ImgResourceLoader>(&path.into(), cx),
            Err(e) => Some(Err(e)),
        }
    })
    .size_full()
    .object_fit(ObjectFit::Contain)
    .with_fallback(placeholder)
    .with_loading(placeholder);
    tile.child(image.id(SharedString::from(format!("project-icon-{}", hit.id))))
}
