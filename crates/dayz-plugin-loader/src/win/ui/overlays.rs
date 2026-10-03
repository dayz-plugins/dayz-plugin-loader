//! Toasts, notices and modal dialogs: the overlay a plugin puts up without owning a panel.
//!
//! These are shared state rather than render-thread state, because a plugin asks for them
//! from whatever thread it is on and the overlay draws them on the next frame. A toast queued
//! before the first frame is therefore not lost — it starts its clock when it is first drawn,
//! which is also what makes the loader's own "ready" toast work at startup.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use dayz_plugin_api::{PluginHandle, UiAnswer, UiDialog, UiLevel, UiNotice};

/// How long a toast stays when the caller does not say.
const TOAST_SECONDS: f32 = 5.0;
/// How long a notice stays when the caller does not say.
const NOTICE_SECONDS: f32 = 3.0;
/// How long the fade at the end takes.
const FADE: f32 = 0.6;

/// What a plugin asked to be shown.
#[derive(Debug, Clone)]
pub(crate) enum Kind {
    /// A corner card or a centred message, with its lifetime in seconds.
    Passing(UiNotice, f32),
    /// A modal dialog, with the text being typed for an input one.
    Modal(UiDialog, String),
}

/// One thing on screen.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    /// Id handed back to the plugin, unique for the process.
    pub(crate) id: u64,
    /// Who asked, or `None` for the loader itself.
    pub(crate) owner: Option<PluginHandle>,
    pub(crate) kind: Kind,
    pub(crate) level: UiLevel,
    pub(crate) title: String,
    pub(crate) text: String,
    pub(crate) accept: String,
    pub(crate) cancel: String,
    /// When it was first drawn. A passing entry queued before the overlay exists waits here.
    pub(crate) shown: Option<Instant>,
}

impl Entry {
    /// Whether this one takes the keyboard and mouse.
    pub(crate) fn is_modal(&self) -> bool {
        matches!(self.kind, Kind::Modal(..))
    }

    /// How opaque it should be drawn, fading out towards the end of its life.
    pub(crate) fn opacity(&self) -> f32 {
        let Kind::Passing(_, seconds) = self.kind else {
            return 1.0;
        };
        let Some(shown) = self.shown else {
            return 1.0;
        };
        let left = seconds - shown.elapsed().as_secs_f32();
        (left / FADE).clamp(0.0, 1.0)
    }

    fn expired(&self) -> bool {
        match self.kind {
            Kind::Passing(_, seconds) => self
                .shown
                .is_some_and(|at| at.elapsed() > Duration::from_secs_f32(seconds)),
            Kind::Modal(..) => false,
        }
    }
}

static ENTRIES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// Whether anything is queued at all, so a frame with nothing to draw costs one atomic read.
static ANY: AtomicBool = AtomicBool::new(false);
/// Whether a modal dialog is open, which is what makes the overlay take input.
static MODAL: AtomicBool = AtomicBool::new(false);

fn entries() -> MutexGuard<'static, Vec<Entry>> {
    ENTRIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn recount(list: &[Entry]) {
    ANY.store(!list.is_empty(), Ordering::Relaxed);
    MODAL.store(list.iter().any(Entry::is_modal), Ordering::Relaxed);
}

/// Whether anything is on screen.
pub(crate) fn any() -> bool {
    ANY.load(Ordering::Relaxed)
}

/// Whether a modal dialog is open.
pub(crate) fn modal() -> bool {
    MODAL.load(Ordering::Relaxed)
}

/// Queue a toast or a notice. Returns its id.
pub(crate) fn passing(
    owner: Option<PluginHandle>,
    kind: UiNotice,
    level: UiLevel,
    title: &str,
    text: &str,
    seconds: f32,
) -> u64 {
    let seconds = if seconds > 0.0 {
        seconds
    } else if kind == UiNotice::Toast {
        TOAST_SECONDS
    } else {
        NOTICE_SECONDS
    };
    add(Entry {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        owner,
        kind: Kind::Passing(kind, seconds),
        level,
        title: title.to_owned(),
        text: text.to_owned(),
        accept: String::new(),
        cancel: String::new(),
        shown: None,
    })
}

/// Queue a modal dialog. Returns its id.
#[allow(clippy::too_many_arguments)]
pub(crate) fn modal_dialog(
    owner: Option<PluginHandle>,
    kind: UiDialog,
    level: UiLevel,
    title: &str,
    text: &str,
    default_text: &str,
    accept: &str,
    cancel: &str,
) -> u64 {
    add(Entry {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        owner,
        kind: Kind::Modal(kind, default_text.to_owned()),
        level,
        title: title.to_owned(),
        text: text.to_owned(),
        accept: accept.to_owned(),
        cancel: cancel.to_owned(),
        shown: None,
    })
}

fn add(entry: Entry) -> u64 {
    let id = entry.id;
    log::debug!(
        "ui {id} {} by {}: {:?} {:?}",
        describe(&entry.kind),
        entry
            .owner
            .map_or_else(|| "loader".to_owned(), |h| h.0.to_string()),
        entry.title,
        entry.text
    );
    let mut list = entries();
    list.push(entry);
    recount(&list);
    id
}

/// What kind of thing an entry is, for the log.
fn describe(kind: &Kind) -> &'static str {
    match kind {
        Kind::Passing(UiNotice::Toast, _) => "toast",
        Kind::Passing(_, _) => "notice",
        Kind::Modal(UiDialog::Message, _) => "message dialog",
        Kind::Modal(UiDialog::Confirm, _) => "confirm dialog",
        Kind::Modal(_, _) => "input dialog",
    }
}

/// A snapshot for the render thread, in the order things were asked for.
pub(crate) fn snapshot() -> Vec<Entry> {
    entries().clone()
}

/// Mark everything in `ids` as drawn, start the clock on the ones that had none, and take
/// away whatever has run out. Returns nothing: an expiring toast has no answer to deliver.
pub(crate) fn mark_shown(ids: &[u64]) {
    let now = Instant::now();
    let mut list = entries();
    for entry in list.iter_mut() {
        if entry.shown.is_none() && ids.contains(&entry.id) {
            entry.shown = Some(now);
            log::debug!("ui {} on screen", entry.id);
        }
    }
    list.retain(|entry| {
        let expired = entry.expired();
        if expired {
            log::debug!("ui {} expired", entry.id);
        }
        !expired
    });
    recount(&list);
}

/// Store what the user has typed into an input dialog, so it survives the next frame.
pub(crate) fn set_input(id: u64, text: &str) {
    let mut list = entries();
    if let Some(entry) = list.iter_mut().find(|e| e.id == id) {
        if let Kind::Modal(_, buffer) = &mut entry.kind {
            if buffer != text {
                text.clone_into(buffer);
            }
        }
    }
}

/// Take one entry out, whoever owns it. Returns it so the caller can answer its owner.
pub(crate) fn take(id: u64) -> Option<Entry> {
    let mut list = entries();
    let index = list.iter().position(|e| e.id == id)?;
    let entry = list.remove(index);
    recount(&list);
    log::debug!("ui {id} closed");
    Some(entry)
}

/// Take one entry out on behalf of its owner, refusing another plugin's id.
pub(crate) fn take_owned(owner: PluginHandle, id: u64) -> Option<Entry> {
    let mut list = entries();
    let index = list
        .iter()
        .position(|e| e.id == id && e.owner == Some(owner))?;
    let entry = list.remove(index);
    recount(&list);
    Some(entry)
}

/// Take everything one plugin owns, for when it stops.
pub(crate) fn take_all(owner: PluginHandle) -> Vec<Entry> {
    let mut list = entries();
    let (mine, rest) = list.iter().cloned().partition(|e| e.owner == Some(owner));
    *list = rest;
    recount(&list);
    mine
}

/// What to tell a plugin about a dialog that is going away.
pub(crate) fn answer_of(
    entry: &Entry,
    answer: UiAnswer,
) -> Option<(PluginHandle, u64, UiAnswer, String)> {
    let Kind::Modal(kind, buffer) = &entry.kind else {
        return None;
    };
    let owner = entry.owner?;
    let text = if *kind == UiDialog::Input && answer == UiAnswer::Accepted {
        buffer.clone()
    } else {
        String::new()
    };
    log::debug!("ui {} answered {answer:?} {text:?}", entry.id);
    Some((owner, entry.id, answer, text))
}
