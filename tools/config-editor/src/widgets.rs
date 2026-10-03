//! One row per ini key: display name, typed value widget, reset buttons, live value and
//! description. Pure drawing; the caller applies the returned edit to the document.

use eframe::egui::{self, Color32, RichText};
use serde_json::Value;

use crate::ini::{Entry, ValueKind};
use crate::live;
use crate::schema::{KeyInfo, KeyType};

const DIRTY: Color32 = Color32::from_rgb(255, 200, 80);
const INVALID: Color32 = Color32::from_rgb(255, 120, 120);
const LIVE_SAME: Color32 = Color32::from_rgb(120, 200, 120);
const LIVE_DIFF: Color32 = Color32::from_rgb(120, 170, 255);
const NAME_WIDTH: f32 = 250.0;

/// Everything a row needs to draw itself.
pub struct Row<'a> {
    pub entry: &'a Entry,
    pub info: Option<&'a KeyInfo>,
    /// Value in the saved file, if the key exists there.
    pub saved: Option<&'a str>,
    /// Value in the running game, if connected and live.
    pub live: Option<&'a Value>,
}

/// Case-insensitive match on key, section, display name and both help texts.
#[must_use]
pub fn matches_filter(entry: &Entry, info: Option<&KeyInfo>, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let mut haystack = format!("{} {} {}", entry.section, entry.key, entry.help);
    if let Some(info) = info {
        haystack.push(' ');
        haystack.push_str(&info.title);
        haystack.push(' ');
        haystack.push_str(&info.description);
    }
    haystack.to_lowercase().contains(filter)
}

/// Draws one key. Returns the new value text when the user changed it.
pub fn entry_row(ui: &mut egui::Ui, row: &Row<'_>) -> Option<String> {
    let dirty = row.saved != Some(row.entry.value.as_str());
    let mut result = None;
    ui.horizontal(|ui| {
        name_label(ui, row, dirty);
        result = value_widget(ui, row);
        if let Some(info) = row.info {
            if !info.unit.is_empty() {
                ui.label(RichText::new(&info.unit).weak());
            }
        }
        if let Some(reset) = reset_buttons(ui, row, dirty) {
            result = Some(reset);
        }
        live_tag(ui, row);
        flags(ui, row);
    });
    let description = row
        .info
        .filter(|info| !info.description.is_empty())
        .map_or(row.entry.help.as_str(), |info| info.description.as_str());
    if !description.is_empty() {
        ui.label(RichText::new(description).weak());
    }
    result
}

fn name_label(ui: &mut egui::Ui, row: &Row<'_>, dirty: bool) {
    let title = row
        .info
        .filter(|info| !info.title.is_empty())
        .map_or(row.entry.key.as_str(), |info| info.title.as_str());
    let mut text = RichText::new(title).strong();
    if dirty {
        text = text.color(DIRTY);
    }
    ui.allocate_ui_with_layout(
        egui::vec2(NAME_WIDTH, 18.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(text)
                .on_hover_text(format!("[{}] {}", row.entry.section, row.entry.key));
        },
    );
}

fn reset_buttons(ui: &mut egui::Ui, row: &Row<'_>, dirty: bool) -> Option<String> {
    let mut result = None;
    if let Some(saved) = row.saved.filter(|_| dirty) {
        if ui
            .small_button("↶")
            .on_hover_text(format!("back to the saved value: {saved}"))
            .clicked()
        {
            result = Some(saved.to_owned());
        }
    }
    if let Some(default) = row.info.and_then(|info| info.default.as_deref()) {
        if default != row.entry.value.trim()
            && ui
                .small_button("default")
                .on_hover_text(format!("reset to the project default: {default}"))
                .clicked()
        {
            result = Some(default.to_owned());
        }
    }
    result
}

fn live_tag(ui: &mut egui::Ui, row: &Row<'_>) {
    let Some(live) = row.live else {
        return;
    };
    let live_text = live::ini_text(live);
    let same = live_text == row.entry.value.trim();
    let tag = RichText::new(format!("live: {live_text}"))
        .small()
        .color(if same { LIVE_SAME } else { LIVE_DIFF });
    ui.label(tag)
        .on_hover_text("value in the running game; Apply pushes the edited value");
}

fn flags(ui: &mut egui::Ui, row: &Row<'_>) {
    let Some(info) = row.info else {
        return;
    };
    if info.unused {
        ui.label(RichText::new("unused").small().color(INVALID))
            .on_hover_text("the current build never reads this key");
    } else if info.restart {
        ui.label(RichText::new("restart").small().weak())
            .on_hover_text("read when the hooks install; takes effect after restarting DayZ");
    } else if info.live {
        ui.label(RichText::new("live").small().weak())
            .on_hover_text("changeable while DayZ runs through the debug plugin");
    }
}

fn value_widget(ui: &mut egui::Ui, row: &Row<'_>) -> Option<String> {
    let kind = row
        .info
        .map_or_else(|| inferred_type(row.entry.kind), |info| info.kind.clone());
    let value = row.entry.value.trim();
    match kind {
        KeyType::Bool => {
            let mut flag = value == "true";
            ui.checkbox(&mut flag, "")
                .changed()
                .then(|| flag.to_string())
        }
        KeyType::Int { min, max, step } => int_widget(ui, value, min, max, step),
        KeyType::Float { min, max, step } => float_widget(ui, value, min, max, step),
        KeyType::Enum(values) => enum_widget(ui, row.entry, &values),
        KeyType::Text => {
            let mut text = row.entry.value.clone();
            ui.add(egui::TextEdit::singleline(&mut text).desired_width(260.0))
                .changed()
                .then_some(text)
        }
    }
}

fn inferred_type(kind: ValueKind) -> KeyType {
    match kind {
        ValueKind::Bool => KeyType::Bool,
        ValueKind::Int => KeyType::Int {
            min: None,
            max: None,
            step: 1,
        },
        ValueKind::Float => KeyType::Float {
            min: None,
            max: None,
            step: 0.1,
        },
        ValueKind::Text => KeyType::Text,
    }
}

fn int_widget(
    ui: &mut egui::Ui,
    value: &str,
    min: Option<i64>,
    max: Option<i64>,
    step: i64,
) -> Option<String> {
    let Ok(mut number) = value.parse::<i64>() else {
        return invalid_text(ui, value);
    };
    let speed = f64::from(u32::try_from(step.max(1)).unwrap_or(u32::MAX));
    let mut drag = egui::DragValue::new(&mut number).speed(speed);
    if let (Some(low), Some(high)) = (min, max) {
        drag = drag.range(low..=high);
    }
    ui.add(drag).changed().then(|| number.to_string())
}

fn float_widget(
    ui: &mut egui::Ui,
    value: &str,
    min: Option<f64>,
    max: Option<f64>,
    step: f64,
) -> Option<String> {
    let Ok(mut number) = value.parse::<f64>() else {
        return invalid_text(ui, value);
    };
    let decimals = decimals_of(value).max(decimals_of(&step.to_string()));
    let mut drag = egui::DragValue::new(&mut number)
        .speed(step)
        .min_decimals(decimals)
        .max_decimals(decimals.max(8));
    if let (Some(low), Some(high)) = (min, max) {
        drag = drag.range(low..=high);
    }
    ui.add(drag)
        .changed()
        .then(|| format!("{number:.decimals$}"))
}

/// A value the schema calls numeric but that does not parse: edit it as red text.
fn invalid_text(ui: &mut egui::Ui, value: &str) -> Option<String> {
    let mut text = value.to_owned();
    ui.add(
        egui::TextEdit::singleline(&mut text)
            .desired_width(120.0)
            .text_color(INVALID),
    )
    .on_hover_text("not a number; the game will fall back to its default")
    .changed()
    .then_some(text)
}

fn enum_widget(ui: &mut egui::Ui, entry: &Entry, values: &[String]) -> Option<String> {
    let current = entry.value.trim().to_owned();
    let mut selected = current.clone();
    let known = values.iter().any(|v| v == &current);
    let shown = if known {
        RichText::new(&current)
    } else {
        RichText::new(format!("{current} (unknown)")).color(INVALID)
    };
    egui::ComboBox::from_id_salt(entry.index)
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for value in values {
                ui.selectable_value(&mut selected, value.clone(), value);
            }
        });
    (selected != current).then_some(selected)
}

fn decimals_of(text: &str) -> usize {
    text.split_once('.').map_or(0, |(_, fraction)| {
        fraction.trim_end_matches(['e', 'E']).len()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_follow_the_written_text() {
        assert_eq!(decimals_of("1.48353004"), 8);
        assert_eq!(decimals_of("0.005"), 3);
        assert_eq!(decimals_of("-600"), 0);
    }

    #[test]
    fn filter_sees_schema_titles() {
        let entry = Entry {
            index: 0,
            section: "stereo".into(),
            key: "lock_yaw".into(),
            value: "false".into(),
            kind: ValueKind::Bool,
            help: String::new(),
        };
        let info = KeyInfo {
            title: "Lock yaw".into(),
            description: "keeps the aim level".into(),
            kind: KeyType::Bool,
            live: true,
            restart: false,
            unused: false,
            unit: String::new(),
            default: Some("false".into()),
        };
        assert!(matches_filter(&entry, Some(&info), "aim level"));
        assert!(matches_filter(&entry, None, "lock_yaw"));
        assert!(!matches_filter(&entry, None, "aim level"));
        assert!(matches_filter(&entry, None, ""));
    }
}
