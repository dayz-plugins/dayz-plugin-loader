//! Resolving the `dayz-data` database against the running executable.
//!
//! The game is already mapped, so the loader scans the live image rather than a file: a
//! pattern hit is then an address it can hand straight to a plugin. The result is computed
//! once, during initialisation, and read without a lock afterwards.

// FFI module: queries the running module's base address and reads its mapped image.
#![allow(unsafe_code)]

use std::path::Path;
use std::sync::OnceLock;

use dayz_data::{Database, Identity, MatchedBy, Origin, Provenance, SymbolTable};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::{GetModuleInformation, MODULEINFO};
use windows::Win32::System::Threading::GetCurrentProcess;

/// What resolution produced, for the host API to read.
pub(crate) struct Resolved {
    /// Base address the executable is mapped at.
    pub(crate) module_base: *mut core::ffi::c_void,
    /// Version of the matched build, empty when the executable is not a known build.
    pub(crate) build: String,
    /// The symbol table.
    pub(crate) table: SymbolTable,
}

// SAFETY: `module_base` is an address, never dereferenced by the loader itself, and the rest
// is plain data that is never mutated after initialisation.
unsafe impl Sync for Resolved {}
// SAFETY: see the `Sync` impl.
unsafe impl Send for Resolved {}

static RESOLVED: OnceLock<Resolved> = OnceLock::new();

/// The resolved database, or `None` before initialisation.
pub(crate) fn resolved() -> Option<&'static Resolved> {
    RESOLVED.get()
}

/// Base address and size of the running executable's mapped image.
fn mapped_image() -> Option<(HMODULE, usize)> {
    // SAFETY: a null module name asks for the executable itself.
    let module = unsafe { GetModuleHandleW(None) }.ok()?;
    let mut info = MODULEINFO::default();
    let size = u32::try_from(core::mem::size_of::<MODULEINFO>()).ok()?;
    // SAFETY: `info` is writable and `size` describes it; the pseudo-handle from
    // GetCurrentProcess needs no release.
    unsafe { GetModuleInformation(GetCurrentProcess(), module, &raw mut info, size) }.ok()?;
    Some((module, info.SizeOfImage as usize))
}

/// Load the database, resolve it against the running image, and log the outcome.
///
/// `exe` is the executable on disk, hashed to find its build file; the bytes that get
/// scanned are the mapped ones.
pub(crate) fn initialize(data_dir: &Path, exe: &Path) {
    let db = Database::load(data_dir);
    for problem in &db.problems {
        log::error!("dayz-data: {problem}");
    }
    if db.builds.is_empty() && db.patterns.symbols.is_empty() {
        log::info!(
            "dayz-data: no database in {}; plugins get no symbols",
            data_dir.display()
        );
        return;
    }
    let Some((module, size)) = mapped_image() else {
        log::error!("dayz-data: cannot find the executable's mapped image");
        return;
    };
    // SAFETY: the whole image is mapped and readable for the process's lifetime; the slice is
    // only read, and never handed out beyond this function.
    let image = unsafe { core::slice::from_raw_parts(module.0.cast::<u8>(), size) };

    let hash = dayz_data::sha256_file(exe)
        .inspect_err(|e| log::warn!("dayz-data: cannot hash {}: {e}", exe.display()))
        .ok();
    let identity = Identity {
        sha256: hash.clone(),
        pe_timestamp: pe_timestamp(image),
        image_size: size as u64,
    };
    let matched = db.build_for_identity(&identity);
    let build = matched.map(|(file, _)| file);
    if let Some((found, how)) = matched {
        let key = match how {
            MatchedBy::Hash => "hash",
            MatchedBy::PeHeaders => "pe timestamp and image size",
        };
        log::info!(
            "dayz-data: build {} ({:?}), matched by {key}, {} cached symbols",
            found.build.version,
            found.build.provenance,
            found.symbols.len()
        );
        if found.build.provenance == Provenance::External {
            log::warn!(
                "dayz-data: build {} comes from another project and has no byte checks, so a \
                 wrong address cannot be caught here",
                found.build.version
            );
        }
    } else {
        log::warn!(
            "dayz-data: unknown build (sha256 {}, pe {:#X}, image {size:#X}); scanning patterns",
            hash.as_deref().unwrap_or("unavailable"),
            identity.pe_timestamp
        );
        let size = std::fs::metadata(exe).map(|m| m.len()).unwrap_or_default();
        let similar = db.similar_builds(size);
        if !similar.is_empty() {
            log::info!("dayz-data: same file size as {}", similar.join(", "));
        }
    }

    let table = SymbolTable::resolve(image, build, &db.patterns);
    report(&table);
    if build.is_none() {
        if let Some(hash) = hash.as_deref() {
            write_candidate(data_dir, exe, hash, &table);
        }
    }
    let resolved = Resolved {
        module_base: module.0,
        build: build.map(|b| b.build.version.clone()).unwrap_or_default(),
        table,
    };
    if RESOLVED.set(resolved).is_err() {
        log::error!("dayz-data: resolved twice");
    }
}

/// PE timestamp from the mapped headers. Zero when they do not look like a PE image.
///
/// Read from memory rather than the file: the headers are mapped at the image base, and this
/// is the identity both predecessor projects gated their hooks on.
fn pe_timestamp(image: &[u8]) -> u64 {
    let read_u32 = |at: usize| -> Option<u32> {
        let bytes: [u8; 4] = image.get(at..at + 4)?.try_into().ok()?;
        Some(u32::from_le_bytes(bytes))
    };
    let Some(nt) = read_u32(0x3C).map(|o| o as usize) else {
        return 0;
    };
    if image.get(nt..nt + 4) != Some(b"PE\0\0") {
        return 0;
    }
    // COFF file header: signature (4) + machine (2) + sections (2), then TimeDateStamp.
    read_u32(nt + 8).map_or(0, u64::from)
}

/// Log one line per symbol, so a game update is diagnosable from the log alone.
fn report(table: &SymbolTable) {
    for (name, resolved) in table.symbols() {
        let origin = match resolved.origin {
            Origin::Cached => "cached",
            Origin::Scanned => "scanned",
        };
        log::debug!("dayz-data: {name} = {:#X} ({origin})", resolved.rva);
    }
    for issue in table.issues() {
        log::warn!("dayz-data: {issue}");
    }
    let scanned = table
        .symbols()
        .filter(|(_, r)| r.origin == Origin::Scanned)
        .count();
    log::info!(
        "dayz-data: {} symbols ({scanned} scanned), {} offsets, {} unresolved",
        table.symbols().count(),
        table.offsets().count(),
        table.issues().len()
    );
}

/// Write what scanning found for an unknown build, so the next launch starts from a cache and
/// the file can be reviewed and contributed upstream.
fn write_candidate(data_dir: &Path, exe: &Path, hash: &str, table: &SymbolTable) {
    if table.symbols().count() == 0 {
        return;
    }
    let file = dayz_data::BuildFile {
        schema: dayz_data::SCHEMA_VERSION,
        build: dayz_data::BuildInfo {
            // The game's own version is not readable from here, so the hash prefix names the
            // file. A person renames it when they confirm which build it is.
            version: format!("unknown-{}", &hash[..hash.len().min(12)]),
            executable: exe.file_name().map_or_else(
                || "DayZ_x64.exe".to_owned(),
                |n| n.to_string_lossy().into_owned(),
            ),
            sha256: Some(hash.to_owned()),
            file_size: std::fs::metadata(exe).map(|m| m.len()).ok(),
            pe_timestamp: None,
            image_size: None,
            verified: None,
            provenance: Provenance::Scan,
        },
        symbols: table
            .symbols()
            .map(|(name, resolved)| {
                let entry = dayz_data::SymbolEntry {
                    rva: resolved.rva,
                    kind: resolved.kind,
                    check: None,
                    note: None,
                };
                (name.to_owned(), entry)
            })
            .collect(),
        // Neither is derivable from a scan: an offset and an event class are facts a person
        // establishes, not things a pattern finds. A candidate file carries what the scan
        // produced and nothing it did not.
        offsets: std::collections::BTreeMap::new(),
        events: std::collections::BTreeMap::new(),
    };
    match Database::write_build(data_dir, &file) {
        Ok(path) => log::info!("dayz-data: wrote candidate {}", path.display()),
        Err(e) => log::warn!("dayz-data: could not write a candidate build file: {e}"),
    }
}

/// Lines for the console's `symbols` command: addresses, then offsets, then anything that
/// did not resolve, filtered by an optional name prefix.
pub(crate) fn console_lines(prefix: Option<&str>) -> Vec<String> {
    let Some(resolved) = resolved() else {
        return Vec::new();
    };
    let matches = |name: &str| prefix.is_none_or(|p| name.starts_with(p));
    let mut lines = Vec::new();
    if prefix.is_none() {
        let build = if resolved.build.is_empty() {
            "unknown build"
        } else {
            &resolved.build
        };
        lines.push(format!("build {build}, base {:p}", resolved.module_base));
    }
    for (name, symbol) in resolved.table.symbols().filter(|(name, _)| matches(name)) {
        let origin = match symbol.origin {
            Origin::Cached => "cached",
            Origin::Scanned => "scanned",
        };
        lines.push(format!("{name} = {:#X} ({origin})", symbol.rva));
    }
    for (name, value) in resolved.table.offsets().filter(|(name, _)| matches(name)) {
        lines.push(format!("{name} = +{value:#X}"));
    }
    for issue in resolved.table.issues().iter().filter(|i| matches(i.name())) {
        lines.push(format!("unresolved: {issue}"));
    }
    lines
}
