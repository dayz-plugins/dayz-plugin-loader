//! Finds `dayz_openxr.ini` when no path is given: beside the executable (the mod's
//! install location), the working directory, the repository (for development builds
//! under `target/`), `DAYZ_DIR`, and the usual Steam library locations.

use std::path::{Path, PathBuf};

pub const INI_NAME: &str = "dayz_openxr.ini";
const STEAM_SUFFIX: &str = "steamapps/common/DayZ";

/// Candidate directories in priority order; may contain duplicates and missing paths.
#[must_use]
pub fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent();
        for _ in 0..5 {
            if let Some(current) = dir {
                dirs.push(current.to_path_buf());
                dir = current.parent();
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    if let Some(game) = std::env::var_os("DAYZ_DIR") {
        dirs.push(PathBuf::from(game));
    }
    dirs.extend(steam_dirs());
    dirs
}

fn steam_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        for var in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Some(root) = std::env::var_os(var) {
                dirs.push(Path::new(&root).join("Steam").join(STEAM_SUFFIX));
            }
        }
    } else if let Some(home) = std::env::var_os("HOME") {
        let home = Path::new(&home);
        dirs.push(home.join(".steam/steam").join(STEAM_SUFFIX));
        dirs.push(home.join(".local/share/Steam").join(STEAM_SUFFIX));
        dirs.push(
            home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam")
                .join(STEAM_SUFFIX),
        );
    }
    dirs
}

/// The first existing ini among the candidate directories.
#[must_use]
pub fn find_default() -> Option<PathBuf> {
    find_in(&candidate_dirs())
}

fn find_in(dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(INI_NAME))
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_existing_ini_wins() {
        let root = std::env::temp_dir().join(format!("dayz-vr-config-{}", std::process::id()));
        let first = root.join("a");
        let second = root.join("b");
        std::fs::create_dir_all(&first).unwrap_or_else(|e| panic!("{e}"));
        std::fs::create_dir_all(&second).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(second.join(INI_NAME), "[x]\n").unwrap_or_else(|e| panic!("{e}"));
        let found = find_in(&[root.join("missing"), first.clone(), second.clone()]);
        assert_eq!(found, Some(second.join(INI_NAME)));
        assert_eq!(find_in(&[first]), None);
        let _ = std::fs::remove_dir_all(root);
    }
}
