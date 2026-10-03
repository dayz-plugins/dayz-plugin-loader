//! Console line parsing and the built-in command set.
//!
//! The console owns no state of its own; the loader resolves names against the plugin,
//! setting and command registries. This module only decides *what* a typed line means.

use thiserror::Error;

/// A parsed console line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// Nothing typed (blank or comment).
    Empty,
    /// `help [name]`.
    Help(Option<String>),
    /// `list [prefix]`: settings and commands, optionally filtered by prefix.
    List(Option<String>),
    /// `get <plugin.key>`.
    Get(String),
    /// `set <plugin.key> <value>`.
    Set(String, String),
    /// `plugins`: loaded plugins with state.
    Plugins,
    /// `<plugin.key>` alone prints the value, `<plugin.key> <value>` sets it.
    Variable(String, Option<String>),
    /// `<plugin.command> [args...]`: forwarded to the owning plugin with raw args.
    Command(String, String),
}

/// Why a line could not be parsed.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    /// `set` without a value.
    #[error("usage: set <plugin.key> <value>")]
    SetNeedsValue,
    /// `get`/`set` without a name.
    #[error("usage: {0} <plugin.key>")]
    NeedsName(&'static str),
}

/// Split the first whitespace separated word from the rest.
fn split_word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    match text.find(char::is_whitespace) {
        Some(i) => (&text[..i], text[i..].trim()),
        None => (text, ""),
    }
}

/// Parse one line. Identifiers are lower-cased; values and arguments keep their case.
///
/// # Errors
/// See [`ParseError`].
pub fn parse(line: &str) -> Result<Line, ParseError> {
    let text = line.trim();
    if text.is_empty() || text.starts_with('#') || text.starts_with("//") {
        return Ok(Line::Empty);
    }
    let (word, rest) = split_word(text);
    let word = word.to_ascii_lowercase();
    let optional = |s: &str| (!s.is_empty()).then(|| s.to_ascii_lowercase());
    Ok(match word.as_str() {
        "help" | "?" => Line::Help(optional(rest)),
        "list" | "ls" => Line::List(optional(rest)),
        "plugins" => Line::Plugins,
        "get" => {
            if rest.is_empty() {
                return Err(ParseError::NeedsName("get"));
            }
            Line::Get(rest.to_ascii_lowercase())
        }
        "set" => {
            let (name, value) = split_word(rest);
            if name.is_empty() {
                return Err(ParseError::NeedsName("set"));
            }
            if value.is_empty() {
                return Err(ParseError::SetNeedsValue);
            }
            Line::Set(name.to_ascii_lowercase(), value.to_owned())
        }
        _ if word.contains('.') => Line::Command(word, rest.to_owned()),
        _ => Line::Variable(word, optional(rest).map(|_| rest.to_owned())),
    })
}

/// Help text for the built-in commands, one entry per line.
pub const BUILTIN_HELP: &[(&str, &str)] = &[
    (
        "help [name]",
        "Show this list, or help for one setting or command.",
    ),
    (
        "list [prefix]",
        "List settings and commands, optionally filtered.",
    ),
    ("plugins", "List loaded plugins and their state."),
    ("get <plugin.key>", "Print a setting."),
    ("set <plugin.key> <value>", "Change a setting."),
    ("<plugin.key> [value]", "Shorthand for get / set."),
    ("<plugin.command> [args]", "Run a plugin command."),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_and_comments_are_empty() {
        for l in ["", "   ", "# note", "// note"] {
            assert_eq!(parse(l), Ok(Line::Empty));
        }
    }

    #[test]
    fn builtins() {
        assert_eq!(parse("HELP"), Ok(Line::Help(None)));
        assert_eq!(
            parse("help dayzvr.stereo.ipd"),
            Ok(Line::Help(Some("dayzvr.stereo.ipd".into())))
        );
        assert_eq!(parse("ls dayzvr"), Ok(Line::List(Some("dayzvr".into()))));
        assert_eq!(parse("plugins"), Ok(Line::Plugins));
        assert_eq!(parse("get A.B"), Ok(Line::Get("a.b".into())));
        assert_eq!(
            parse("set a.b Hello World"),
            Ok(Line::Set("a.b".into(), "Hello World".into()))
        );
        assert_eq!(parse("set a.b"), Err(ParseError::SetNeedsValue));
        assert_eq!(parse("get"), Err(ParseError::NeedsName("get")));
    }

    #[test]
    fn dotted_words_are_commands_or_variables() {
        assert_eq!(
            parse("dayzvr.recenter"),
            Ok(Line::Command("dayzvr.recenter".into(), String::new()))
        );
        assert_eq!(
            parse("Dayzvr.Spawn M4A1 2"),
            Ok(Line::Command("dayzvr.spawn".into(), "M4A1 2".into()))
        );
        assert_eq!(parse("fps"), Ok(Line::Variable("fps".into(), None)));
        assert_eq!(
            parse("fps 60"),
            Ok(Line::Variable("fps".into(), Some("60".into())))
        );
    }
}
