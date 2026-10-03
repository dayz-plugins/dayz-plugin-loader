//! Comment-preserving model of `dayz_openxr.ini`.
//!
//! The file is the mod's documentation: every key carries the comment block above it.
//! The editor therefore never rewrites the file from a map; it keeps every line and only
//! replaces the value text of the entries the user changed.

use std::fmt::Write as _;

/// One physical line of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    Blank,
    Comment(String),
    /// Header with its name and the exact original text.
    Section {
        name: String,
        raw: String,
    },
    Entry {
        key: String,
        separator: String,
        value: String,
    },
    /// Anything the parser does not understand is kept verbatim.
    Other(String),
}

/// How a value should be edited. Inferred from the text currently in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Bool,
    Int,
    Float,
    Text,
}

impl ValueKind {
    #[must_use]
    pub fn infer(value: &str) -> Self {
        let trimmed = value.trim();
        if matches!(trimmed, "true" | "false") {
            Self::Bool
        } else if trimmed.parse::<i64>().is_ok() {
            Self::Int
        } else if trimmed.parse::<f64>().is_ok() {
            Self::Float
        } else {
            Self::Text
        }
    }
}

/// A key in a section, with its position in the document and its comment block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Index into the document's line list; stable while the document is not re-parsed.
    pub index: usize,
    pub section: String,
    pub key: String,
    pub value: String,
    pub kind: ValueKind,
    /// Comment lines directly above the key, without the `#`/`;` markers.
    pub help: String,
}

impl Entry {
    /// `section.key`, the name the debug plugin uses for live tunables.
    #[must_use]
    pub fn tunable_name(&self) -> String {
        format!("{}.{}", self.section, self.key)
    }
}

/// A section header and the entries under it, in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub entries: Vec<Entry>,
}

/// Errors from editing a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The line index does not refer to a `key=value` line.
    NotAnEntry(usize),
    /// Values are single-line; a newline would corrupt the file.
    MultiLine,
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnEntry(index) => write!(f, "line {index} is not a key=value entry"),
            Self::MultiLine => f.write_str("a value cannot contain a line break"),
        }
    }
}

impl std::error::Error for EditError {}

/// The parsed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    lines: Vec<Line>,
    newline: &'static str,
    trailing_newline: bool,
}

impl Document {
    /// Parses the text; never fails, unknown lines are kept as they are.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let trailing_newline = text.ends_with('\n');
        let body = text.strip_suffix('\n').unwrap_or(text);
        let body = body.strip_suffix('\r').unwrap_or(body);
        let lines = if body.is_empty() && !trailing_newline {
            Vec::new()
        } else {
            body.split('\n')
                .map(|line| line.strip_suffix('\r').unwrap_or(line))
                .map(parse_line)
                .collect()
        };
        Self {
            lines,
            newline,
            trailing_newline,
        }
    }

    /// Serialises the document with the original line endings.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for (position, line) in self.lines.iter().enumerate() {
            if position > 0 {
                out.push_str(self.newline);
            }
            match line {
                Line::Blank => {}
                Line::Comment(text) | Line::Other(text) => out.push_str(text),
                Line::Section { raw, .. } => out.push_str(raw),
                Line::Entry {
                    key,
                    separator,
                    value,
                } => {
                    let _ = write!(out, "{key}{separator}{value}");
                }
            }
        }
        if self.trailing_newline && !self.lines.is_empty() {
            out.push_str(self.newline);
        }
        out
    }

    /// Sections with their entries, in file order. Keys before the first header are
    /// reported under an empty section name.
    #[must_use]
    pub fn sections(&self) -> Vec<Section> {
        let mut sections: Vec<Section> = Vec::new();
        let mut current = String::new();
        let mut help: Vec<String> = Vec::new();
        for (index, line) in self.lines.iter().enumerate() {
            match line {
                Line::Section { name, .. } => {
                    current.clone_from(name);
                    sections.push(Section {
                        name: name.clone(),
                        entries: Vec::new(),
                    });
                    help.clear();
                }
                Line::Comment(text) => help.push(strip_comment_marker(text)),
                Line::Blank | Line::Other(_) => help.clear(),
                Line::Entry { key, value, .. } => {
                    let entry = Entry {
                        index,
                        section: current.clone(),
                        key: key.clone(),
                        value: value.clone(),
                        kind: ValueKind::infer(value),
                        help: help.join(" "),
                    };
                    help.clear();
                    match sections.last_mut() {
                        Some(section) if section.name == current => section.entries.push(entry),
                        _ => sections.push(Section {
                            name: current.clone(),
                            entries: vec![entry],
                        }),
                    }
                }
            }
        }
        sections
    }

    /// Current value of a key, if present.
    #[must_use]
    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        let mut current = "";
        for line in &self.lines {
            match line {
                Line::Section { name, .. } => current = name,
                Line::Entry {
                    key: entry_key,
                    value,
                    ..
                } if current == section && entry_key == key => return Some(value),
                _ => {}
            }
        }
        None
    }

    /// Replaces the value text of the entry at `index`.
    ///
    /// # Errors
    /// Returns an error when `index` is not an entry or the value spans lines.
    pub fn set(&mut self, index: usize, new_value: &str) -> Result<(), EditError> {
        if new_value.contains(['\n', '\r']) {
            return Err(EditError::MultiLine);
        }
        match self.lines.get_mut(index) {
            Some(Line::Entry { value, .. }) => {
                new_value.clone_into(value);
                Ok(())
            }
            _ => Err(EditError::NotAnEntry(index)),
        }
    }
}

fn parse_line(line: &str) -> Line {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Line::Blank;
    }
    if trimmed.starts_with('#') || trimmed.starts_with(';') {
        return Line::Comment(line.to_owned());
    }
    if let Some(inner) = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        return Line::Section {
            name: inner.trim().to_owned(),
            raw: line.to_owned(),
        };
    }
    if let Some(equals) = line.find('=') {
        let (left, right) = line.split_at(equals);
        let key = left.trim_end();
        if key.trim().is_empty() {
            return Line::Other(line.to_owned());
        }
        let value_start = right[1..].len() - right[1..].trim_start().len();
        let separator = format!("{}={}", &left[key.len()..], &right[1..=value_start]);
        return Line::Entry {
            key: key.to_owned(),
            separator,
            value: right[1 + value_start..].to_owned(),
        };
    }
    Line::Other(line.to_owned())
}

fn strip_comment_marker(text: &str) -> String {
    text.trim_start()
        .trim_start_matches(['#', ';'])
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "[openxr]\nenabled=true\n\n[stereo]\n# Signed counts per radian.\n# Invert to flip.\nhmd_mouse_yaw_scale=-600\nfit_mode = stretch\nscale_x=1.0\n";

    #[test]
    fn round_trip_is_byte_identical() {
        let doc = Document::parse(SAMPLE);
        assert_eq!(doc.to_text(), SAMPLE);
        let crlf = SAMPLE.replace('\n', "\r\n");
        assert_eq!(Document::parse(&crlf).to_text(), crlf);
        let no_trailing = SAMPLE.trim_end();
        assert_eq!(Document::parse(no_trailing).to_text(), no_trailing);
        assert_eq!(Document::parse("").to_text(), "");
    }

    #[test]
    fn sections_carry_help_and_kinds() {
        let doc = Document::parse(SAMPLE);
        let sections = doc.sections();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].name, "openxr");
        assert_eq!(sections[0].entries[0].kind, ValueKind::Bool);
        let stereo = &sections[1];
        assert_eq!(
            stereo.entries[0].help,
            "Signed counts per radian. Invert to flip."
        );
        assert_eq!(stereo.entries[0].kind, ValueKind::Int);
        assert_eq!(
            stereo.entries[0].tunable_name(),
            "stereo.hmd_mouse_yaw_scale"
        );
        assert_eq!(stereo.entries[1].help, "");
        assert_eq!(stereo.entries[1].kind, ValueKind::Text);
        assert_eq!(stereo.entries[2].kind, ValueKind::Float);
    }

    #[test]
    fn set_replaces_only_the_value_and_keeps_spacing() {
        let mut doc = Document::parse(SAMPLE);
        let sections = doc.sections();
        let fit = &sections[1].entries[1];
        doc.set(fit.index, "cover")
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(doc.to_text().contains("fit_mode = cover\n"));
        assert_eq!(doc.get("stereo", "fit_mode"), Some("cover"));
        assert_eq!(doc.get("stereo", "missing"), None);
        assert_eq!(doc.set(0, "x"), Err(EditError::NotAnEntry(0)));
        assert_eq!(doc.set(fit.index, "a\nb"), Err(EditError::MultiLine));
    }

    #[test]
    fn odd_lines_survive() {
        let text = "garbage line\n=novalue\n[weird ]\nkey=\n";
        let doc = Document::parse(text);
        assert_eq!(doc.to_text(), text);
        let sections = doc.sections();
        assert_eq!(sections[0].name, "weird");
        assert_eq!(sections[0].entries[0].value, "");
    }
}
