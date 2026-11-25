use std::time::{Duration, Instant};

use super::Page;
use ely_gpui_component::{
    motion::{ProgressBar, ease_out_cubic, spring},
    theme::{ActiveTheme, Theme},
};
use gpui::{prelude::*, *};

pub(super) const ROW: f32 = 60.;
pub(super) const GAP: f32 = 4.;
const GLIDE: Duration = Duration::from_millis(360);
const ENTER: f32 = 0.30;
const DELAY: f32 = 0.045;

pub(super) fn sync_preference(cx: &mut App) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
        };
        let mut enabled = 1i32;
        // SPI_GETCLIENTAREAANIMATION writes a Win32 BOOL to the supplied pointer.
        let ok = unsafe {
            SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut enabled).cast(), 0)
        };
        if ok != 0 && cx.theme().reduced_motion != (enabled == 0) {
            Theme::update(cx, |theme| theme.reduced_motion = enabled == 0);
        }
    }
    #[cfg(not(windows))]
    let _ = cx;
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Destination {
    pub page: Page,
    pub instance: usize,
    pub packs: bool,
}

pub(super) struct Navigation {
    target: Option<Destination>,
    page_since: Instant,
    rail_since: Instant,
    rail_from: f32,
}
#[derive(Clone, Copy)]
pub(super) struct Frame {
    elapsed: f32,
    pub row: f32,
}
impl Default for Navigation {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            target: None,
            page_since: now,
            rail_since: now,
            rail_from: 0.,
        }
    }
}
impl Navigation {
    fn row_at(&self, now: Instant, to: usize) -> f32 {
        let t = (now.duration_since(self.rail_since).as_secs_f32() / GLIDE.as_secs_f32()).min(1.);
        self.rail_from + (to as f32 - self.rail_from) * spring(t)
    }
    pub fn frame(&mut self, next: Destination, reduced: bool, now: Instant) -> Frame {
        if self.target != Some(next) {
            self.rail_from = self
                .target
                .map(|v| self.row_at(now, v.instance))
                .unwrap_or(next.instance as f32);
            self.rail_since = now;
            self.page_since = now;
            self.target = Some(next);
        }
        if reduced {
            self.rail_from = next.instance as f32;
        }
        Frame {
            row: if reduced {
                next.instance as f32
            } else {
                self.row_at(now, next.instance)
            },
            elapsed: if reduced {
                1.
            } else {
                now.duration_since(self.page_since).as_secs_f32()
            },
        }
    }
}
impl Frame {
    pub fn active(self) -> bool {
        self.elapsed < ENTER + 3. * DELAY
    }
    pub fn section(self, el: Div, order: u8) -> Div {
        let t = ease_out_cubic(((self.elapsed - DELAY * order as f32) / ENTER).clamp(0., 1.));
        el.relative()
            .top(px(16. * (1. - t)))
            .opacity(0.10 + 0.90 * t)
    }
}

pub(super) fn task_progress(status: &str) -> ProgressBar {
    let Some((stage, count)) = status.rsplit_once(" · ") else {
        return ProgressBar::indeterminate("pending-work");
    };
    let ratio = count
        .split_once(" / ")
        .and_then(|(a, b)| Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?)));
    match ratio {
        Some((done, total)) if total > 0 && done <= total => ProgressBar::new(
            SharedString::from(format!("progress-{stage}")),
            done as f32 / total as f32,
        ),
        _ => ProgressBar::indeterminate("pending-work"),
    }
}
