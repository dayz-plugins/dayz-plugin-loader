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

mod chrome;
mod console;
mod forward;
mod frame;
mod icons;
mod overlays;
mod paint;
mod render;
mod settings_panel;
mod wnd;

use core::ffi::c_void;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use dayz_plugin_core::windows::Geometry;
use egui::{Context, Pos2, Rect, Vec2};
use windows::Win32::Graphics::Direct3D11::ID3D11Device;

pub(crate) use frame::widget;
pub(crate) use overlays::{answer_of, modal_dialog, passing, take_all, take_owned};

use super::{dispatch, state};

/// Whether the overlay currently has the keyboard and mouse.
static CAPTURING: AtomicBool = AtomicBool::new(false);
/// Whether the overlay is *willing* to hold the input while its windows are open.
///
/// Open windows normally take the keyboard and mouse, which is what makes them usable and
/// what stops a typed console line from also walking the player forward. That is the wrong
/// answer for a panel somebody wants to *watch* while playing — a HUD, a map, a readout — so
/// the `loader.mouse` hotkey clears this and the windows stay on screen with the game back in
/// control. Set again on the next press, and whenever the last window closes, so the hotkey
/// never has to be pressed twice to get a working panel.
static GRABBED: AtomicBool = AtomicBool::new(true);
/// Whether there is anything at all to draw. A toast is visible without capturing.
static VISIBLE: AtomicBool = AtomicBool::new(false);
/// Whether the loader's own console panel is open.
static CONSOLE_OPEN: AtomicBool = AtomicBool::new(false);
/// Whether the loader's settings editor is open.
static EDITOR_OPEN: AtomicBool = AtomicBool::new(false);
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
    console: console::ConsolePanel,
    editor: settings_panel::SettingsPanel,
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
/// Key presses the game window received since the last call, as `(scan code, virtual key)`.
pub(crate) fn key_presses() -> Vec<(u16, u16)> {
    wnd::take_presses()
}

pub(crate) fn toggle_console() {
    let open = !CONSOLE_OPEN.load(Ordering::Relaxed);
    CONSOLE_OPEN.store(open, Ordering::Relaxed);
    log::debug!("in-game console {}", if open { "opened" } else { "closed" });
    if open {
        // A console nobody can type into is not a console: opening it always takes the input
        // back, however the mouse was left after the last panel.
        GRABBED.store(true, Ordering::Relaxed);
    }
    // The key that opened this also produced a character; it belongs to the game, not to the
    // input box that is about to take the keyboard.
    wnd::flush();
    refresh();
}

/// Take the mouse and keyboard, or give them back to the game, leaving the windows up.
pub(crate) fn toggle_grab() {
    let grabbed = !GRABBED.load(Ordering::Relaxed);
    GRABBED.store(grabbed, Ordering::Relaxed);
    recount();
    if grabbed {
        wnd::centre_pointer();
    }
    let open = VISIBLE.load(Ordering::Relaxed);
    log::info!(
        "overlay {} the mouse",
        if grabbed { "took" } else { "released" }
    );
    // Said out loud, because the overlay looks the same either way and a panel that has
    // stopped reacting to clicks is otherwise indistinguishable from a panel that has hung.
    let text = if grabbed {
        "The overlay has the mouse and keyboard."
    } else {
        "The game has the mouse and keyboard. Panels stay on screen."
    };
    if open {
        loader_toast(dayz_plugin_api::UiLevel::Info, "Input", text);
    }
}

/// Open or close the loader's settings editor.
pub(crate) fn toggle_editor() {
    let open = !EDITOR_OPEN.load(Ordering::Relaxed);
    EDITOR_OPEN.store(open, Ordering::Relaxed);
    log::debug!("settings editor {}", if open { "opened" } else { "closed" });
    wnd::flush();
    recount();
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
    let was = PANELS_OPEN.swap(open, Ordering::Relaxed);
    let nothing_open = open == 0 && !CONSOLE_OPEN.load(Ordering::Relaxed);
    if nothing_open {
        GRABBED.store(true, Ordering::Relaxed);
    } else if open > was {
        // A window that has just appeared is one somebody wants to use.
        if !GRABBED.swap(true, Ordering::Relaxed) {
            wnd::centre_pointer();
        }
    }
    recount();
}

/// Recompute what is on screen and what has the input.
///
/// Split in two because a toast is visible without taking the keyboard: only windows and a
/// modal dialog capture, everything else merely draws.
pub(crate) fn recount() {
    let windows = PANELS_OPEN.load(Ordering::Relaxed) > 0
        || CONSOLE_OPEN.load(Ordering::Relaxed)
        || EDITOR_OPEN.load(Ordering::Relaxed);
    // A modal dialog captures whatever the mouse toggle says: it is a question a plugin is
    // waiting on an answer to, and there is no way to answer it without the pointer.
    let grabbed = GRABBED.load(Ordering::Relaxed);
    CAPTURING.store((windows && grabbed) || overlays::modal(), Ordering::Relaxed);
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
        console: console::ConsolePanel::default(),
        editor: settings_panel::SettingsPanel::default(),
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
        let frame = self.build(input);
        let Some(output) = frame.output else {
            return Ok(());
        };

        let (renderer_output, platform, _viewports) = egui_directx11::split_output(output);
        // egui asks its host to do the copying; with no backend under us, that is this.
        for command in platform.commands {
            if let egui::OutputCommand::CopyText(text) = command {
                super::clipboard::set_text(&text);
            }
        }
        let saved = render::save(&device_context);
        let result =
            self.renderer
                .render(&device_context, &target.view, &self.ctx, renderer_output);
        render::restore(&device_context, &saved);
        result?;

        // After the frame, so nothing below runs while a panel body is on the stack.
        apply(&frame.closed);
        if let Some(line) = frame.submitted {
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
    fn build(&mut self, input: egui::RawInput) -> Frame {
        let panels = state().panel_list();
        let console_open = CONSOLE_OPEN.load(Ordering::Relaxed);
        // Only while it is open: both are a clone of everything the console can show, and a
        // closed console has nobody to show it to.
        let (scrollback, names) = if console_open {
            (console_lines(), state().completion_names())
        } else {
            (Vec::new(), Vec::new())
        };
        let ctx = self.ctx.clone();
        let was_shown = self.console_shown;
        self.console_shown = CONSOLE_OPEN.load(Ordering::Relaxed);
        let just_opened = self.console_shown && !was_shown;
        let console = &mut self.console;
        if just_opened {
            console.opened();
        }
        let mut frame = Frame::default();
        let queued = overlays::snapshot();
        let editor_open = EDITOR_OPEN.load(Ordering::Relaxed);
        let saved = Saved::of(&panels, editor_open);
        let (sections, advanced) = (&saved.sections, saved.advanced);
        let geometry_of = |id: &str| saved.geometry_of(id);
        let pressed = editor_open.then(wnd::take_last_key).flatten();
        let editor = &mut self.editor;
        let mut placements: Vec<(String, Geometry)> = Vec::new();
        let mut edits = settings_panel::Edits::default();
        let mut painted = paint::Outcome::default();
        let output = ctx.run_ui(input, |ui| {
            let ctx = ui.ctx().clone();
            if console_open {
                let chrome = chrome::Chrome {
                    id: "loader.console",
                    title: "DayZ plugin loader",
                    default_size: [720.0, 420.0],
                    saved: geometry_of("loader.console"),
                };
                let outcome = console.show(&ctx, &scrollback, &names, &chrome);
                frame.submitted = outcome.submitted;
                place(&mut placements, chrome.id, &outcome.placed);
                if !outcome.placed.open || outcome.close {
                    CONSOLE_OPEN.store(false, Ordering::Relaxed);
                }
            }
            for (handle, plugin, panel, title, open) in &panels {
                if !*open {
                    continue;
                }
                let qualified = format!("{plugin}.{panel}");
                let chrome = chrome::Chrome {
                    id: &qualified,
                    title,
                    default_size: [320.0, 240.0],
                    saved: geometry_of(&qualified),
                };
                let placed = chrome::show(&ctx, &chrome, |ui| {
                    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed) as u64;
                    frame::with(token, ui, || dispatch::ui(*handle, &qualified, token));
                });
                place(&mut placements, &qualified, &placed);
                if !placed.open {
                    frame.closed.push((*handle, panel.clone()));
                }
            }
            if editor_open {
                let chrome = chrome::Chrome {
                    id: "loader.settings",
                    title: "Loader settings",
                    default_size: [560.0, 420.0],
                    saved: geometry_of("loader.settings"),
                };
                let view = settings_panel::View {
                    sections,
                    pressed,
                    advanced,
                };
                let (made, placed) = editor.show(&ctx, &view, &chrome);
                edits = made;
                place(&mut placements, chrome.id, &placed);
                if !placed.open {
                    EDITOR_OPEN.store(false, Ordering::Relaxed);
                }
            }
            painted = paint::draw(&ctx, &queued);
            if is_capturing() {
                cursor(&ctx);
            } else {
                released(&ctx, &saved.mouse_binding);
            }
        });
        resolve(&painted);
        apply_edits(&edits);
        remember(&placements, ctx.input(|i| i.pointer.any_down()));
        frame.output = Some(output);
        frame
    }
}

/// What the loader state held when the frame began.
///
/// Read once, before the pass: every window needs something out of the state, and the lock is
/// never held while a panel body — which calls into a plugin — is on the stack.
struct Saved {
    /// Remembered geometry per window id, in the order the windows are drawn.
    geometry: Vec<(String, Option<Geometry>)>,
    /// The settings editor's snapshot, empty when it is closed.
    sections: Vec<crate::state::EditorSection>,
    /// Whether advanced settings are shown.
    advanced: bool,
    /// What `loader.mouse` is bound to, for the hint shown while the game has the input.
    mouse_binding: String,
}

impl Saved {
    fn of(
        panels: &[(dayz_plugin_api::PluginHandle, String, String, String, bool)],
        editor_open: bool,
    ) -> Self {
        let guard = state();
        let ids = ["loader.console".to_owned(), "loader.settings".to_owned()]
            .into_iter()
            .chain(
                panels
                    .iter()
                    .map(|(_, plugin, panel, _, _)| format!("{plugin}.{panel}")),
            );
        let mouse_binding = guard
            .hotkeys
            .iter()
            .find(|entry| entry.name == "loader.mouse")
            .map_or_else(
                || "none".to_owned(),
                dayz_plugin_core::hotkeys::Entry::binding,
            );
        Saved {
            geometry: ids
                .map(|id| (guard.windows.get(&id), id))
                .map(|(g, id)| (id, g))
                .collect(),
            sections: if editor_open {
                guard.editor_snapshot()
            } else {
                Vec::new()
            },
            advanced: guard.windows.advanced(),
            mouse_binding,
        }
    }

    fn geometry_of(&self, id: &str) -> Option<Geometry> {
        self.geometry
            .iter()
            .find(|(saved_id, _)| saved_id == id)
            .and_then(|(_, geometry)| *geometry)
    }
}

/// What one egui pass produced, for the caller to act on after it has ended.
#[derive(Default)]
struct Frame {
    /// The pass itself, for the renderer.
    output: Option<egui::FullOutput>,
    /// A console line the user submitted.
    submitted: Option<String>,
    /// Panels the user closed, as `(owner, panel)`.
    closed: Vec<(dayz_plugin_api::PluginHandle, String)>,
}

/// Note where a window ended up, when it had a rectangle this frame.
fn place(into: &mut Vec<(String, Geometry)>, id: &str, placed: &chrome::Placed) {
    if let Some(geometry) = placed.geometry {
        into.push((id.to_owned(), geometry));
    }
}

/// Hand the frame's window positions to the layout, and save it once the mouse is let go.
///
/// Waiting for the button is what keeps a drag from writing the file sixty times a second;
/// it is also the moment a person has decided where the window goes.
fn remember(placements: &[(String, Geometry)], pointer_down: bool) {
    let mut guard = state();
    for (id, geometry) in placements {
        guard.windows.remember(id, *geometry);
    }
    if !pointer_down {
        guard.save_windows();
    }
}

/// Write back what the settings editor changed.
///
/// Through the same calls the console makes, so a slider and a typed `set` cannot disagree,
/// and with no lock held across the plugin notification that follows a setting change.
fn apply_edits(edits: &settings_panel::Edits) {
    for (name, value) in &edits.settings {
        let outcome = state().set_setting(None, name, value);
        match outcome {
            Ok(notify) => {
                log::debug!("editor set {name} = {value}");
                dispatch::deliver(notify.into_iter().collect());
            }
            Err((_, message)) => log::warn!("editor could not set {name}: {message}"),
        }
    }
    if let Some(advanced) = edits.advanced {
        let mut guard = state();
        guard.windows.set_advanced(advanced);
        guard.save_windows();
    }
    for (action, chord) in &edits.bindings {
        let binding = chord.map_or_else(|| "none".to_owned(), |c| c.to_string());
        if state().rebind_hotkey(action, *chord) {
            log::info!("rebound {action} to {binding}");
        } else {
            log::warn!("no such action: {action}");
        }
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

/// Say that the game has the input, and which key takes it back.
///
/// Only while windows are open, so the line does not sit over a toast on its own. Drawn in
/// the same layer as the cursor it replaces, above everything else.
fn released(ctx: &Context, binding: &str) {
    if !VISIBLE.load(Ordering::Relaxed) || PANELS_OPEN.load(Ordering::Relaxed) == 0 {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("dayz-loader-released"),
    ));
    let at = Pos2::new(ctx.viewport_rect().center().x, 12.0);
    let text = format!("The game has the mouse — {binding} to take it back");
    let galley = painter.layout_no_wrap(
        text,
        egui::FontId::proportional(13.0),
        egui::Color32::from_gray(210),
    );
    let rect = Rect::from_center_size(
        Pos2::new(at.x, at.y + galley.size().y / 2.0),
        galley.size() + Vec2::new(16.0, 8.0),
    );
    painter.rect_filled(rect, 4.0, egui::Color32::from_black_alpha(160));
    painter.galley(rect.min + Vec2::new(8.0, 4.0), galley, egui::Color32::WHITE);
}

/// The console scrollback, as a snapshot taken without the lock held afterwards.
fn console_lines() -> Vec<crate::scrollback::Line> {
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
