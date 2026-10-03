//! Enforces the project's file and function length limits for this crate.

use std::path::Path;

const MAX_FILE_LINES: usize = 1000;
const MAX_FN_LINES: usize = 100;

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Longest function body in lines, counting from a line that declares `fn` and opens a
/// block to its matching closing brace (brace counting; strings are not parsed).
fn longest_fn(source: &str) -> (usize, usize) {
    let mut longest = (0, 0);
    let lines: Vec<&str> = source.lines().collect();
    for (start, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let is_fn = trimmed.starts_with("fn ")
            || trimmed.starts_with("pub fn ")
            || trimmed.starts_with("pub(crate) fn ");
        if !is_fn || !line.trim_end().ends_with('{') {
            continue;
        }
        let mut depth = 0_i32;
        for (offset, body) in lines[start..].iter().enumerate() {
            depth += i32::try_from(body.matches('{').count()).unwrap_or(0);
            depth -= i32::try_from(body.matches('}').count()).unwrap_or(0);
            if depth == 0 {
                if offset + 1 > longest.0 {
                    longest = (offset + 1, start + 1);
                }
                break;
            }
        }
    }
    longest
}

#[test]
fn files_and_functions_stay_small() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("tests"), &mut files);
    assert!(!files.is_empty(), "no Rust sources found");
    for path in files {
        let source = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{e}"));
        let lines = source.lines().count();
        assert!(
            lines <= MAX_FILE_LINES,
            "{} has {lines} lines (limit {MAX_FILE_LINES})",
            path.display()
        );
        let (length, at) = longest_fn(&source);
        assert!(
            length <= MAX_FN_LINES,
            "{}:{at} function is {length} lines (limit {MAX_FN_LINES})",
            path.display()
        );
    }
}
