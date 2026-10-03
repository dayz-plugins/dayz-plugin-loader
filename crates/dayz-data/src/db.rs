//! Loading a database directory, and hashing an executable to find its build file.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::schema::{BuildFile, PatternFile};
use crate::{BUILDS_DIR, PATTERNS_FILE, SCHEMA_VERSION};

/// Why a database could not be loaded.
#[derive(Debug, Error)]
pub enum DatabaseError {
    /// A file could not be read.
    #[error("{path}: {source}")]
    Io {
        /// File involved.
        path: String,
        /// Underlying error.
        source: io::Error,
    },
    /// A file is not valid JSON, or does not match the schema.
    #[error("{path}: {source}")]
    Parse {
        /// File involved.
        path: String,
        /// Underlying error.
        source: serde_json::Error,
    },
    /// A file was written by a newer version of this format.
    #[error("{path}: schema {found} is newer than {SCHEMA_VERSION}, which this build understands")]
    Schema {
        /// File involved.
        path: String,
        /// The version the file claims.
        found: u32,
    },
}

/// Hash an executable, so its build file can be found.
///
/// # Errors
/// The file could not be read.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    // 1 MiB at a time: enough that the read syscall is not the bottleneck on a ~20 MB
    // executable, small enough to stay off the stack and out of the way.
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// A loaded database: the patterns plus every build file found.
#[derive(Debug, Clone, Default)]
pub struct Database {
    /// Build files, in the order they were read.
    pub builds: Vec<BuildFile>,
    /// Signatures, empty when the directory has no pattern file.
    pub patterns: PatternFile,
    /// Files that could not be read or parsed. One bad file must not hide the rest, so these
    /// are collected and reported rather than returned as a failure.
    pub problems: Vec<String>,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, DatabaseError> {
    let text = fs::read_to_string(path).map_err(|source| DatabaseError::Io {
        path: path.display().to_string(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| DatabaseError::Parse {
        path: path.display().to_string(),
        source,
    })
}

/// Reject a file from a newer schema instead of guessing at its contents.
fn check_schema(path: &Path, found: u32) -> Result<(), DatabaseError> {
    if found > SCHEMA_VERSION {
        return Err(DatabaseError::Schema {
            path: path.display().to_string(),
            found,
        });
    }
    Ok(())
}

impl Database {
    /// Load `patterns.json` and every file under `builds/`. A missing directory is an empty
    /// database, not an error: the loader must still start without one.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        let mut db = Database::default();
        let patterns_path = dir.join(PATTERNS_FILE);
        if patterns_path.exists() {
            match read_json::<PatternFile>(&patterns_path)
                .and_then(|f| check_schema(&patterns_path, f.schema).map(|()| f))
            {
                Ok(file) => db.patterns = file,
                Err(e) => db.problems.push(e.to_string()),
            }
        }
        let Ok(entries) = fs::read_dir(dir.join(BUILDS_DIR)) else {
            return db;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("json"))
            })
            .collect();
        files.sort();
        for path in files {
            match read_json::<BuildFile>(&path)
                .and_then(|f| check_schema(&path, f.schema).map(|()| f))
            {
                Ok(file) => db.builds.push(file),
                Err(e) => db.problems.push(e.to_string()),
            }
        }
        db
    }

    /// Merge another database into this one. Entries already present win, which is what
    /// makes a user directory override the copy shipped with the loader.
    pub fn merge_under(&mut self, other: Database) {
        for build in other.builds {
            if !self
                .builds
                .iter()
                .any(|b| b.build.sha256 == build.build.sha256)
            {
                self.builds.push(build);
            }
        }
        for (name, entry) in other.patterns.symbols {
            self.patterns.symbols.entry(name).or_insert(entry);
        }
        self.problems.extend(other.problems);
    }

    /// The build file for an executable hash, compared case-insensitively.
    #[must_use]
    pub fn build_for(&self, sha256: &str) -> Option<&BuildFile> {
        self.builds
            .iter()
            .find(|b| b.build.sha256.eq_ignore_ascii_case(sha256))
    }

    /// Builds whose file size matches but whose hash does not: the nearest misses, named in
    /// the log so a person has somewhere to start after a game update.
    #[must_use]
    pub fn similar_builds(&self, file_size: u64) -> Vec<&str> {
        self.builds
            .iter()
            .filter(|b| b.build.file_size == file_size)
            .map(|b| b.build.version.as_str())
            .collect()
    }

    /// Write a build file into `builds/`, named after its version.
    ///
    /// # Errors
    /// The directory could not be created or the file could not be written.
    pub fn write_build(dir: &Path, file: &BuildFile) -> Result<PathBuf, DatabaseError> {
        let builds = dir.join(BUILDS_DIR);
        let io_err = |path: &Path| {
            let path = path.display().to_string();
            move |source| DatabaseError::Io {
                path: path.clone(),
                source,
            }
        };
        fs::create_dir_all(&builds).map_err(io_err(&builds))?;
        let path = builds.join(format!("{}.json", file.build.version));
        let mut text =
            serde_json::to_string_pretty(file).map_err(|source| DatabaseError::Parse {
                path: path.display().to_string(),
                source,
            })?;
        text.push('\n');
        fs::write(&path, text).map_err(io_err(&path))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{BuildInfo, Provenance};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("dayz-data-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(path.join(BUILDS_DIR)).unwrap_or_else(|e| panic!("{e}"));
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn build_json(version: &str, sha: &str, size: u64, schema: u32) -> String {
        format!(
            r#"{{"schema":{schema},"build":{{"version":"{version}","executable":"DayZ_x64.exe",
               "sha256":"{sha}","file_size":{size}}},"symbols":{{"a":{{"rva":"0x10"}}}}}}"#
        )
    }

    #[test]
    fn missing_directory_is_an_empty_database() {
        let db = Database::load(Path::new("/nonexistent/dayz-data"));
        assert!(db.builds.is_empty());
        assert!(db.patterns.symbols.is_empty());
        assert!(db.problems.is_empty());
        assert_eq!(db.build_for("abc"), None);
    }

    #[test]
    fn loads_patterns_and_builds_and_keeps_bad_files_as_problems() {
        let dir = TempDir::new("load");
        fs::write(
            dir.0.join(PATTERNS_FILE),
            r#"{"schema":1,"symbols":{"a":{"patterns":["48 8B"]}}}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        fs::write(
            dir.0.join(BUILDS_DIR).join("good.json"),
            build_json("1.29", "AABB", 10, 1),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        fs::write(dir.0.join(BUILDS_DIR).join("broken.json"), "{ not json")
            .unwrap_or_else(|e| panic!("{e}"));
        fs::write(
            dir.0.join(BUILDS_DIR).join("future.json"),
            build_json("9.9", "CCDD", 10, 99),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        fs::write(
            dir.0.join(BUILDS_DIR).join("ignored.txt"),
            "not a json file",
        )
        .unwrap_or_else(|e| panic!("{e}"));

        let db = Database::load(&dir.0);
        assert_eq!(db.patterns.symbols.len(), 1);
        assert_eq!(db.builds.len(), 1);
        assert_eq!(db.problems.len(), 2, "the broken and the too-new file");
        assert!(db.problems.iter().any(|p| p.contains("schema 99 is newer")));
        assert!(
            db.build_for("aabb").is_some(),
            "hash comparison ignores case"
        );
        assert_eq!(db.similar_builds(10), ["1.29"]);
        assert!(db.similar_builds(11).is_empty());
    }

    #[test]
    fn merging_keeps_the_entries_already_present() {
        let mut user = Database {
            builds: vec![
                serde_json::from_str(&build_json("1.29-user", "AABB", 10, 1))
                    .unwrap_or_else(|e| panic!("{e}")),
            ],
            patterns: serde_json::from_str(
                r#"{"schema":1,"symbols":{"a":{"patterns":["11 22"]}}}"#,
            )
            .unwrap_or_else(|e| panic!("{e}")),
            problems: Vec::new(),
        };
        let bundled = Database {
            builds: vec![
                serde_json::from_str(&build_json("1.29-bundled", "AABB", 10, 1))
                    .unwrap_or_else(|e| panic!("{e}")),
                serde_json::from_str(&build_json("1.28", "CCDD", 9, 1))
                    .unwrap_or_else(|e| panic!("{e}")),
            ],
            patterns: serde_json::from_str(
                r#"{"schema":1,"symbols":{"a":{"patterns":["33 44"]},"b":{"patterns":["55 66"]}}}"#,
            )
            .unwrap_or_else(|e| panic!("{e}")),
            problems: vec!["a problem".into()],
        };
        user.merge_under(bundled);
        assert_eq!(
            user.build_for("AABB").map(|b| b.build.version.as_str()),
            Some("1.29-user")
        );
        assert_eq!(
            user.builds.len(),
            2,
            "the build only the bundle had came through"
        );
        assert_eq!(user.patterns.symbols["a"].patterns[0].to_string(), "11 22");
        assert!(user.patterns.symbols.contains_key("b"));
        assert_eq!(user.problems, ["a problem"]);
    }

    #[test]
    fn written_build_files_can_be_read_back() {
        let dir = TempDir::new("write");
        let file = BuildFile {
            schema: SCHEMA_VERSION,
            build: BuildInfo {
                version: "1.30.0".into(),
                executable: "DayZ_x64.exe".into(),
                sha256: "ff".into(),
                file_size: 5,
                pe_timestamp: None,
                image_size: None,
                verified: None,
                provenance: Provenance::Scan,
            },
            symbols: std::collections::BTreeMap::new(),
            offsets: std::collections::BTreeMap::new(),
        };
        let path = Database::write_build(&dir.0, &file).unwrap_or_else(|e| panic!("{e}"));
        assert!(path.ends_with("builds/1.30.0.json"));
        let db = Database::load(&dir.0);
        assert_eq!(db.builds, vec![file]);
    }

    #[test]
    fn hashing_matches_the_reference_value() {
        let dir = TempDir::new("hash");
        let path = dir.0.join("empty");
        fs::write(&path, b"").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            sha256_file(&path).ok().as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert!(sha256_file(Path::new("/nonexistent")).is_err());
    }
}
