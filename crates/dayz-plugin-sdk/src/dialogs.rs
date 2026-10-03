//! Toasts, notices and dialogs a plugin can put on screen without owning a panel.
//!
//! The loader draws and owns all of them. A toast and a notice are fire and forget; a dialog
//! is answered later, in [`Plugin::on_dialog`](crate::Plugin::on_dialog), because the player
//! takes as long as they take and nothing may block a frame waiting for them.

use dayz_plugin_api::{Str, UiDialog, UiLevel, UiNotice};

/// A message that puts itself away: a toast in the corner or a notice in the middle.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    kind: UiNotice,
    level: UiLevel,
    title: String,
    text: String,
    seconds: f32,
}

impl Notice {
    fn new(kind: UiNotice, text: &str) -> Self {
        Notice {
            kind,
            level: UiLevel::Info,
            title: String::new(),
            text: text.to_owned(),
            seconds: 0.0,
        }
    }

    /// A card in the corner, stacked under whatever is already there.
    #[must_use]
    pub fn toast(text: &str) -> Self {
        Self::new(UiNotice::Toast, text)
    }

    /// Large text in the middle of the screen, for the few things that must be seen.
    ///
    /// Named `centred` rather than `notice` because `Notice::notice` reads like a mistake;
    /// the ABI's name for the kind is still `Notice`.
    #[must_use]
    pub fn centred(text: &str) -> Self {
        Self::new(UiNotice::Notice, text)
    }

    /// A bold first line.
    #[must_use]
    pub fn title(mut self, title: &str) -> Self {
        title.clone_into(&mut self.title);
        self
    }

    /// Colour and tone.
    #[must_use]
    pub fn level(mut self, level: UiLevel) -> Self {
        self.level = level;
        self
    }

    /// How long it stays. Zero, the default, means the loader decides.
    #[must_use]
    pub fn seconds(mut self, seconds: f32) -> Self {
        self.seconds = seconds;
        self
    }

    pub(crate) fn to_api(&self) -> dayz_plugin_api::NoticeDesc {
        dayz_plugin_api::NoticeDesc {
            struct_size: core::mem::size_of::<dayz_plugin_api::NoticeDesc>(),
            kind: self.kind,
            level: self.level,
            title: Str::new(&self.title),
            text: Str::new(&self.text),
            seconds: self.seconds,
        }
    }
}

/// A modal dialog. The answer arrives in `on_dialog`.
#[derive(Debug, Clone, PartialEq)]
pub struct Dialog {
    kind: UiDialog,
    level: UiLevel,
    title: String,
    text: String,
    default_text: String,
    accept: String,
    cancel: String,
}

impl Dialog {
    fn new(kind: UiDialog, title: &str, text: &str) -> Self {
        Dialog {
            kind,
            level: UiLevel::Info,
            title: title.to_owned(),
            text: text.to_owned(),
            default_text: String::new(),
            accept: String::new(),
            cancel: String::new(),
        }
    }

    /// Text and one button.
    #[must_use]
    pub fn message(title: &str, text: &str) -> Self {
        Self::new(UiDialog::Message, title, text)
    }

    /// Text, a confirming button and a cancelling one.
    #[must_use]
    pub fn confirm(title: &str, text: &str) -> Self {
        Self::new(UiDialog::Confirm, title, text)
    }

    /// Text and one line to type into.
    #[must_use]
    pub fn input(title: &str, text: &str) -> Self {
        Self::new(UiDialog::Input, title, text)
    }

    /// What the input line starts out containing.
    #[must_use]
    pub fn default_text(mut self, text: &str) -> Self {
        text.clone_into(&mut self.default_text);
        self
    }

    /// Replace the default button labels.
    #[must_use]
    pub fn buttons(mut self, accept: &str, cancel: &str) -> Self {
        accept.clone_into(&mut self.accept);
        cancel.clone_into(&mut self.cancel);
        self
    }

    /// Colour and tone.
    #[must_use]
    pub fn level(mut self, level: UiLevel) -> Self {
        self.level = level;
        self
    }

    pub(crate) fn to_api(&self) -> dayz_plugin_api::DialogDesc {
        dayz_plugin_api::DialogDesc {
            struct_size: core::mem::size_of::<dayz_plugin_api::DialogDesc>(),
            kind: self.kind,
            level: self.level,
            title: Str::new(&self.title),
            text: Str::new(&self.text),
            default_text: Str::new(&self.default_text),
            accept_label: Str::new(&self.accept),
            cancel_label: Str::new(&self.cancel),
        }
    }
}

/// What the loader hands back for something it put on screen.
///
/// Pass it to [`Host::close`](crate::Host::close) to take it down early, and compare it
/// against the one `on_dialog` reports to tell your dialogs apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Shown(pub u64);
