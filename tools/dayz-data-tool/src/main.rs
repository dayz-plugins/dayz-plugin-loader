//! Command line tool for the `dayz-data` database.
//!
//! ```text
//! dayz-data inspect  --exe <DayZ_x64.exe>
//! dayz-data validate <dir> [--exe <DayZ_x64.exe>]
//! dayz-data generate <dir> --exe <DayZ_x64.exe> --seed <seed.json> [--date YYYY-MM-DD]
//! ```
//!
//! `inspect` prints what identifies a build. `validate` reads a database directory and, with
//! an executable, resolves it and reports every symbol. `generate` turns a seed file of
//! known addresses into a build file and candidate patterns.

mod generate;
mod image;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dayz_data::{Database, Origin, PatternFile, SymbolTable, PATTERNS_FILE, SCHEMA_VERSION};
use dayz_plugin_core::cmdline::CommandLine;

const USAGE: &str = "\
dayz-data inspect  --exe <DayZ_x64.exe>
dayz-data validate <dir> [--exe <DayZ_x64.exe>]
dayz-data generate <dir> --exe <DayZ_x64.exe> --seed <seed.json> [--date YYYY-MM-DD]";

fn main() -> ExitCode {
    let args = CommandLine::parse(std::env::args().skip(1));
    let positionals: Vec<&str> = args.positionals().collect();
    let result = match positionals.first().copied() {
        Some("inspect") => inspect(&args),
        Some("validate") => validate(&args, positionals.get(1).copied()),
        Some("generate") => generate_files(&args, positionals.get(1).copied()),
        Some(other) => Err(format!("unknown command {other:?}\n\n{USAGE}")),
        None => Err(USAGE.to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// Read a required option.
fn option<'a>(args: &'a CommandLine, name: &str) -> Result<&'a str, String> {
    args.value(name)
        .ok_or_else(|| format!("--{name} is required\n\n{USAGE}"))
}

/// Read the executable named by `--exe`, hash it and map it out.
fn executable(args: &CommandLine) -> Result<(PathBuf, String, image::Image), String> {
    let path = PathBuf::from(option(args, "exe")?);
    let sha256 = dayz_data::sha256_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mapped = image::load(&path)?;
    Ok((path, sha256, mapped))
}

fn inspect(args: &CommandLine) -> Result<(), String> {
    let (path, sha256, mapped) = executable(args)?;
    let name = path
        .file_name()
        .map_or_else(|| "?".into(), |n| n.to_string_lossy().into_owned());
    println!("executable   {name}");
    println!("sha256       {sha256}");
    println!("file size    {}", mapped.file_size);
    println!("pe timestamp {:#X}", mapped.timestamp);
    println!("image size   {:#X}", mapped.size_of_image);
    Ok(())
}

fn validate(args: &CommandLine, dir: Option<&str>) -> Result<(), String> {
    let dir = Path::new(dir.ok_or_else(|| format!("a database directory is required\n\n{USAGE}"))?);
    let db = Database::load(dir);
    println!(
        "{} build files, {} patterns",
        db.builds.len(),
        db.patterns.symbols.len()
    );
    for build in &db.builds {
        println!(
            "  {} {} symbols, {} offsets, {:?}, sha256 {:?}",
            build.build.version,
            build.symbols.len(),
            build.offsets.len(),
            build.build.provenance,
            build.build.sha256
        );
    }
    for problem in &db.problems {
        println!("problem: {problem}");
    }
    if args.has("exe") {
        resolve_against(args, &db)?;
    }
    if db.problems.is_empty() {
        Ok(())
    } else {
        Err(format!("{} file(s) could not be used", db.problems.len()))
    }
}

/// Resolve the database against an executable and print the result per symbol.
fn resolve_against(args: &CommandLine, db: &Database) -> Result<(), String> {
    let (_, sha256, mapped) = executable(args)?;
    let build = db.build_for(&sha256);
    if let Some(found) = build {
        println!("\nbuild {} matches by hash", found.build.version);
    } else {
        println!("\nno build file for sha256 {sha256}");
        let similar = db.similar_builds(mapped.file_size);
        if !similar.is_empty() {
            println!("same file size as: {}", similar.join(", "));
        }
    }
    let table = SymbolTable::resolve(&mapped.bytes, build, &db.patterns);
    for (name, resolved) in table.symbols() {
        let origin = match resolved.origin {
            Origin::Cached => "cached",
            Origin::Scanned => "scanned",
        };
        println!("  {name:<32} {:#010X}  {origin}", resolved.rva);
    }
    for (name, value) in table.offsets() {
        println!("  {name:<32} +{value:#X}");
    }
    for issue in table.issues() {
        println!("  issue: {issue}");
    }
    println!(
        "{} symbols, {} offsets, {} issues",
        table.symbols().count(),
        table.offsets().count(),
        table.issues().len()
    );
    let disagreements = cross_check(&mapped.bytes, db, &table);
    if !disagreements.is_empty() {
        for line in &disagreements {
            println!("  {line}");
        }
        return Err(format!(
            "{} symbol(s) where the cache and the pattern disagree",
            disagreements.len()
        ));
    }
    Ok(())
}

/// Compare the cached addresses against what the patterns alone find.
///
/// The patterns are the source of truth and the build file is a cache, so a disagreement means
/// one of the two is stale: typically a seed address that moved while the pattern kept the byte
/// run of the old one. Nothing else in the pipeline notices that, because each half verifies
/// only against itself.
fn cross_check(image: &[u8], db: &Database, cached: &SymbolTable) -> Vec<String> {
    let scanned = SymbolTable::resolve(image, None, &db.patterns);
    cached
        .symbols()
        .filter_map(|(name, resolved)| {
            let found = scanned.symbol(name)?;
            (found.rva != resolved.rva).then(|| {
                format!(
                    "mismatch: {name} is {:#X} in the build file but its pattern finds {:#X}",
                    resolved.rva, found.rva
                )
            })
        })
        .collect()
}

fn generate_files(args: &CommandLine, dir: Option<&str>) -> Result<(), String> {
    let dir = Path::new(dir.ok_or_else(|| format!("a database directory is required\n\n{USAGE}"))?);
    let (path, sha256, mapped) = executable(args)?;
    let seed_path = option(args, "seed")?;
    let seed_text = std::fs::read_to_string(seed_path).map_err(|e| format!("{seed_path}: {e}"))?;
    let seed: generate::Seed =
        serde_json::from_str(&seed_text).map_err(|e| format!("{seed_path}: {e}"))?;
    let today = args.value("date").unwrap_or("unverified");
    let executable_name = path.file_name().map_or_else(
        || "DayZ_x64.exe".into(),
        |n| n.to_string_lossy().into_owned(),
    );

    let (build, patterns, report) =
        generate::generate(&seed, &mapped, &executable_name, &sha256, today);
    let written = Database::write_build(dir, &build).map_err(|e| e.to_string())?;
    println!(
        "wrote {} ({} symbols, {} offsets)",
        written.display(),
        build.symbols.len(),
        build.offsets.len()
    );

    let merged = merge_patterns(dir, patterns)?;
    println!(
        "wrote {} ({merged} patterns)",
        dir.join(PATTERNS_FILE).display()
    );
    println!("{} symbols got a candidate pattern", report.patterned.len());
    if !report.not_unique.is_empty() {
        println!(
            "no unique byte run, address only: {}",
            report.not_unique.join(", ")
        );
    }
    if !report.out_of_image.is_empty() {
        return Err(format!(
            "seed addresses outside the image: {}",
            report.out_of_image.join(", ")
        ));
    }
    println!(
        "\nGenerated patterns are literal byte runs, so they may contain operands that change\n\
         between builds. Replace those bytes with ?? by hand to make a signature durable."
    );
    Ok(())
}

/// Add generated patterns to the directory's pattern file, keeping the ones already there:
/// a hand-wildcarded signature must never be overwritten by a fresh literal run.
fn merge_patterns(dir: &Path, generated: PatternFile) -> Result<usize, String> {
    let path = dir.join(PATTERNS_FILE);
    let mut existing: PatternFile = if path.exists() {
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?
    } else {
        PatternFile {
            schema: SCHEMA_VERSION,
            symbols: std::collections::BTreeMap::new(),
        }
    };
    for (name, entry) in generated.symbols {
        existing.symbols.entry(name).or_insert(entry);
    }
    let mut text = serde_json::to_string_pretty(&existing).map_err(|e| e.to_string())?;
    text.push('\n');
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(existing.symbols.len())
}
