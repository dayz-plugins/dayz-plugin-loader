//! The in-game overlay: `egui`, a `Direct3D` 11 renderer, and the panels plugins register.
//!
//! The loader owns the UI library and no plugin ever sees it. A plugin registers a panel and
//! fills its body through [`frame::widget`], which means the loader keeps the device, the
//! frame lifetime and the fault guard — and means the same panel can be rasterised onto a
//! world-space quad for VR later without a plugin changing a line.
//!
//! Everything here runs on the thread that calls `Present`, which is why the overlay lives in
//! a thread local rather than a static: the `Direct3D` device context is not thread safe, and
//! this way it is structurally impossible to touch it from anywhere else. The only state
//! shared with other threads is the handful of atomics below and the loader's own state.

mod console_panel;
mod frame;
mod overlays;
mod paint;
mod render;
mod wnd;

use core::ffi::c_void;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use egui::{Context, Pos2, Rect, Vec2};
use windows::Win32::Graphics::Direct3D11::ID3D11Device;

pub(crate) use frame::widget;
pub(crate) use overlays::{answer_of, modal_dialog, passing, take_all, take_owned};

use super::{dispatch, state};

/// Whether the overlay currently has the keyboard and mouse.
static CAPTURING: AtomicBool = AtomicBool::new(false);
/// Whether there is anything at all to draw. A toast is visible without capturing.
static VISIBLE: AtomicBool = AtomicBool::new(false);
/// Whether the loader's own console panel is open.
static CONSOLE_OPEN: AtomicBool = AtomicBool::new(false);
/// How many plugin panels are open, so `Present` can leave immediately when none are.
static PANELS_OPEN: AtomicUsize = AtomicUsize::new(0);
/// Hands out the per-call tokens that stand in for a panel body.
static NEXT_TOKEN: AtomicUsize = AtomicUsize::new(1);

/// How many console lines the panel is given each frame.
const SCROLLBACK: usize = 400;

thread_local! {
    /// The overlay, created on the first frame that needs it.
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

/// Everything the overlay needs between frames.
struct Overlay {
    ctx: Context,
    renderer: egui_directx11::Renderer,
    device: ID3D11Device,
    console: console_panel::ConsolePanel,
    /// Whether the console panel was drawn last frame, so opening it can take the keyboard.
    console_shown: bool,
    started: Instant,
    scale: f32,
}

/// Whether the overlay is taking input, which is what the window subclass asks.
pub(crate) fn is_capturing() -> bool {
    CAPTURING.load(Ordering::Relaxed)
}

/// Hook the game's window so the overlay can receive input.
///
/// Called when the swapchain appears, possibly on another thread than `Present`; it only
/// touches atomics and the window, never the overlay.
pub(crate) fn on_swapchain(hwnd: *mut c_void) {
    wnd::subclass(hwnd);
}

/// Open or close the loader's console panel.
pub(crate) fn toggle_console() {
    let open = !CONSOLE_OPEN.load(Ordering::Relaxed);
    CONSOLE_OPEN.store(open, Ordering::Relaxed);
    log::debug!("in-game console {}", if open { "opened" } else { "closed" });
    // The key that opened this also produced a character; it belongs to the game, not to the
    // input box that is about to take the keyboard.
    wnd::flush();
    refresh();
}

/// A panel was toggled from a hotkey: forget the keystroke that did it, then recount.
pub(crate) fn opened() {
    wnd::flush();
    refresh();
}

/// Recount what is open. Called whenever a panel or the console is opened or closed.
pub(crate) fn refresh() {
    let open = state()
        .panel_list()
        .iter()
        .filter(|(_, _, _, _, open)| *open)
        .count();
    PANELS_OPEN.store(open, Ordering::Relaxed);
    recount();
}

/// Recompute what is on screen and what has the input.
///
/// Split in two because a toast is visible without taking the keyboard: only windows and a
/// modal dialog capture, everything else merely draws.
pub(crate) fn recount() {
    let windows = PANELS_OPEN.load(Ordering::Relaxed) > 0 || CONSOLE_OPEN.load(Ordering::Relaxed);
    CAPTURING.store(windows || overlays::modal(), Ordering::Relaxed);
    VISIBLE.store(windows || overlays::any(), Ordering::Relaxed);
}

/// Draw one frame, from inside the `Present` hook.
pub(crate) fn present(swapchain: *mut c_void) {
    if !VISIBLE.load(Ordering::Relaxed) {
        return;
    }
    let Some(chain) = render::swapchain_of(swapchain) else {
        return;
    };
    OVERLAY.with_borrow_mut(|slot| {
        let overlay = match slot {
            Some(overlay) => overlay,
            None => match create(&chain) {
                Some(new) => slot.insert(new),
                None => return,
            },
        };
        if let Err(e) = overlay.draw(&chain) {
            log::error!("overlay frame failed: {e}");
        }
    });
}

/// Build the overlay from the device behind the swapchain.
fn create(chain: &windows::Win32::Graphics::Dxgi::IDXGISwapChain) -> Option<Overlay> {
    let device = render::device_of(chain)?;
    let renderer = egui_directx11::Renderer::new(&device)
        .inspect_err(|e| log::error!("could not create the overlay renderer: {e}"))
        .ok()?;
    let ctx = Context::default();
    ctx.set_theme(egui::Theme::Dark);
    log::info!("overlay ready");
    Some(Overlay {
        ctx,
        renderer,
        device,
        console: console_panel::ConsolePanel::default(),
        console_shown: false,
        started: Instant::now(),
        scale: 1.0,
    })
}

impl Overlay {
    fn draw(
        &mut self,
        chain: &windows::Win32::Graphics::Dxgi::IDXGISwapChain,
    ) -> windows::core::Result<()> {
        let target = render::target(chain, &self.device)?;
        let Some(device_context) = render::context(&self.device) else {
            return Ok(());
        };
        #[allow(clippy::cast_precision_loss)]
        let (width, height) = (target.size.0 as f32, target.size.1 as f32);
        self.rescale(height);
        wnd::set_screen(width, height);

        let input = wnd::take_input(
            Rect::from_min_size(Pos2::ZERO, Vec2::new(width, height)),
            self.started.elapsed().as_secs_f64(),
        );
        let (output, submitted, closed) = self.build(input);

        let (renderer_output, _platform, _viewports) = egui_directx11::split_output(output);
        let saved = render::save(&device_context);
        let result =
            self.renderer
                .render(&device_context, &target.view, &self.ctx, renderer_output);
        render::restore(&device_context, &saved);
        result?;

        // After the frame, so nothing below runs while a panel body is on the stack.
        apply(&closed);
        if let Some(line) = submitted {
            super::run_console_line(None, &line);
        }
        Ok(())
    }

    /// Match egui's scale to the backbuffer, so a 4K window does not get a 1080p-sized UI.
    fn rescale(&mut self, height: f32) {
        let wanted = (height / 1080.0).clamp(1.0, 2.5);
        if (wanted - self.scale).abs() > 0.01 {
            self.ctx.set_pixels_per_point(wanted);
            self.scale = wanted;
        }
    }

    /// Run the egui pass: the console, every open panel, and the overlay's own cursor.
    fn build(
        &mut self,
        input: egui::RawInput,
    ) -> (
        egui::FullOutput,
        Option<String>,
        Vec<(dayz_plugin_api::PluginHandle, String)>,
    ) {
        let panels = state().panel_list();
        let scrollback = console_lines();
        let ctx = self.ctx.clone();
        let was_shown = self.console_shown;
        self.console_shown = CONSOLE_OPEN.load(Ordering::Relaxed);
        let just_opened = self.console_shown && !was_shown;
        let console = &mut self.console;
        if just_opened {
            console.opened();
        }
        let mut submitted = None;
        let mut closed = Vec::new();
        let queued = overlays::snapshot();
        let mut painted = paint::Outcome::default();
        let output = ctx.run_ui(input, |ui| {
            let ctx = ui.ctx().clone();
            let mut console_open = CONSOLE_OPEN.load(Ordering::Relaxed);
            if console_open {
                submitted = console.show(&ctx, &scrollback, &mut console_open);
                if !console_open {
                    CONSOLE_OPEN.store(false, Ordering::Relaxed);
                }
            }
            for (handle, plugin, panel, title, open) in &panels {
                if !*open {
                    continue;
                }
                let mut keep = true;
                let qualified = format!("{plugin}.{panel}");
                egui::Window::new(title)
                    .id(egui::Id::new(&qualified))
                    .open(&mut keep)
                    .show(&ctx, |ui| {
                        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed) as u64;
                        frame::with(token, ui, || dispatch::ui(*handle, &qualified, token));
                    });
                if !keep {
                    closed.push((*handle, panel.clone()));
                }
            }
            painted = paint::draw(&ctx, &queued);
            cursor(&ctx);
        });
        resolve(&painted);
        (output, submitted, closed)
    }
}

/// Apply what one frame of toasts, notices and dialogs decided.
///
/// Called after the egui pass, never during it: answering a dialog calls into a plugin, and
/// no plugin is called while a frame body is on the stack.
fn resolve(painted: &paint::Outcome) {
    for (id, text) in &painted.typed {
        overlays::set_input(*id, text);
    }
    for (id, answer) in &painted.answered {
        let Some(entry) = overlays::take(*id) else {
            continue;
        };
        if let Some((owner, id, answer, text)) = overlays::answer_of(&entry, *answer) {
            dispatch::dialog(owner, id, answer, &text);
        }
    }
    overlays::mark_shown(&painted.shown);
    recount();
}

/// Put a toast up on behalf of the loader itself rather than a plugin.
pub(crate) fn loader_toast(level: dayz_plugin_api::UiLevel, title: &str, text: &str) {
    overlays::passing(
        None,
        dayz_plugin_api::UiNotice::Toast,
        level,
        title,
        text,
        0.0,
    );
    recount();
}

/// Take down everything a plugin left on screen, telling it about each dialog.
pub(crate) fn close_all_for(owner: dayz_plugin_api::PluginHandle) {
    for entry in take_all(owner) {
        if let Some((owner, id, answer, text)) =
            overlays::answer_of(&entry, dayz_plugin_api::UiAnswer::Closed)
        {
            dispatch::dialog(owner, id, answer, &text);
        }
    }
    recount();
}

/// Draw the overlay's own pointer.
///
/// The game hides and recentres the system cursor while it has the mouse, so the only
/// reliable cursor while a panel is open is one the overlay draws itself.
fn cursor(ctx: &Context) {
    let at = wnd::pointer() / ctx.pixels_per_point();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("dayz-loader-cursor"),
    ));
    let arrow = [
        at,
        at + Vec2::new(0.0, 16.0),
        at + Vec2::new(4.5, 11.5),
        at + Vec2::new(11.0, 11.0),
    ];
    painter.add(egui::Shape::convex_polygon(
        arrow.to_vec(),
        egui::Color32::WHITE,
        egui::Stroke::new(1.0, egui::Color32::BLACK),
    ));
}

/// The console scrollback, as a snapshot taken without the lock held afterwards.
fn console_lines() -> Vec<String> {
    let guard = state();
    let total = guard.console.len();
    guard
        .console
        .iter()
        .skip(total.saturating_sub(SCROLLBACK))
        .cloned()
        .collect()
}

/// Write back the panels the user closed with the window's own close button.
fn apply(closed: &[(dayz_plugin_api::PluginHandle, String)]) {
    if closed.is_empty() {
        return;
    }
    {
        let mut guard = state();
        for (handle, panel) in closed {
            log::debug!("panel {panel} closed from its window");
            guard.set_panel_open(*handle, panel, false);
        }
    }
    refresh();
}
