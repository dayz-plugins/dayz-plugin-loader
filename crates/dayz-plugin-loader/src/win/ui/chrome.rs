//! The window every overlay window is drawn in.
//!
//! One place for what all of them share: the remembered position, size and opacity, a title
//! bar with the opacity slider beside the close button, and the geometry handed back so the
//! caller can save it. egui's own title bar cannot hold a widget, so the window is drawn
//! without one and this draws it instead; a window with no title bar is still movable by
//! dragging it, which is why the body keeps working the way it did.

use dayz_plugin_core::windows::{Geometry, MIN_ALPHA};
use egui::{Align, Context, Layout, RichText, Slider};

/// Width of the opacity slider in the title bar, in points.
const SLIDER_WIDTH: f32 = 64.0;

/// What a window needs to know about itself before it is drawn.
pub(super) struct Chrome<'a> {
    /// Stable id: what the layout remembers this window by, `loader.console` or
    /// `<plugin>.<panel>`.
    pub id: &'a str,
    /// Title bar text.
    pub title: &'a str,
    /// Size the window takes the first time it is ever opened.
    pub default_size: [f32; 2],
    /// Where it was left, when it has been opened before.
    pub saved: Option<Geometry>,
}

/// What drawing it produced.
pub(super) struct Placed {
    /// Where the window ended up, to hand to the layout. `None` when it was collapsed away
    /// by egui and has no rectangle this frame.
    pub geometry: Option<Geometry>,
    /// Cleared when the user clicked the close button.
    pub open: bool,
}

/// Draw one window: chrome here, contents in `body`.
pub(super) fn show(ctx: &Context, chrome: &Chrome<'_>, body: impl FnOnce(&mut egui::Ui)) -> Placed {
    let saved = chrome.saved.map(Geometry::clamped);
    let alpha = saved.map_or(1.0, |g| g.alpha);
    let mut window = egui::Window::new(chrome.title)
        .id(egui::Id::new(chrome.id))
        .title_bar(false)
        .resizable(true)
        .frame(egui::Frame::window(&ctx.global_style()).multiply_with_opacity(alpha))
        .default_size(chrome.default_size);
    if let Some(geometry) = saved {
        window = window
            .default_pos([geometry.x, geometry.y])
            .default_size([geometry.w, geometry.h]);
    }
    let mut open = true;
    let mut opacity = alpha;
    let response = window.show(ctx, |ui| {
        // The title bar keeps full opacity on purpose: a window someone made nearly
        // transparent must still be findable and grabbable.
        title_bar(ui, chrome.title, &mut opacity, &mut open);
        ui.separator();
        ui.multiply_opacity(alpha);
        body(ui);
    });
    let geometry = response.map(|response| {
        let rect = response.response.rect;
        Geometry {
            x: rect.min.x,
            y: rect.min.y,
            w: rect.width(),
            h: rect.height(),
            alpha: opacity,
        }
    });
    Placed { geometry, open }
}

/// Title, then the opacity slider and the close button, right-aligned.
fn title_bar(ui: &mut egui::Ui, title: &str, opacity: &mut f32, open: &mut bool) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if super::icons::close(ui, "Close") {
                *open = false;
            }
            ui.spacing_mut().slider_width = SLIDER_WIDTH;
            let percent = (*opacity * 100.0).round();
            ui.add(
                Slider::new(opacity, MIN_ALPHA..=1.0)
                    .show_value(false)
                    .trailing_fill(true),
            )
            .on_hover_text(format!("Opacity {percent:.0}%"));
        });
    });
}
