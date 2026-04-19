use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use hangul_conv::{ZipOptions, default_output_for, write_zip};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick};
use walkdir::WalkDir;

/// Tools for working with Korean filenames across macOS and Windows.
///
/// macOS stores Hangul filenames as decomposed jamo (NFD). Windows, Linux,
/// and most archive tools expect precomposed syllables (NFC). The mismatch
/// shows up as "broken" Korean text on Windows.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Rename files/directories from NFD to NFC in place.
    Rename(RenameArgs),
    /// Create a Windows-compatible zip from a folder (NFC names, UTF-8 flag).
    Zip(ZipArgs),
}

#[derive(Parser, Debug)]
struct RenameArgs {
    /// Paths to process (files or directories).
    #[arg(required = true)]
    paths: Vec<PathBuf>,

    /// Recurse into subdirectories.
    #[arg(short, long)]
    recursive: bool,

    /// Show what would change without renaming.
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Suppress per-file output; only print summary.
    #[arg(short, long)]
    quiet: bool,
}

#[derive(Parser, Debug)]
struct ZipArgs {
    /// Folder to zip.
    folder: PathBuf,

    /// Output zip path. Defaults to "<folder-name>.zip" next to the folder.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Deflate compression level (0–9). 0 = store only, 9 = smallest.
    #[arg(short = 'l', long, default_value_t = 6, value_parser = clap::value_parser!(u8).range(0..=9))]
    level: u8,

    /// Include macOS cruft (.DS_Store, __MACOSX/, ._* AppleDouble files).
    #[arg(long)]
    include_mac_cruft: bool,

    /// Do not wrap contents in a top-level folder named after the source.
    #[arg(long)]
    no_wrap: bool,

    /// Suppress per-entry output; only print summary.
    #[arg(short, long)]
    quiet: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Rename(args) => cmd_rename(args),
        Cmd::Zip(args) => cmd_zip(args),
    }
}

// -------- rename --------

#[derive(Default, Debug)]
struct RenameStats {
    scanned: usize,
    renamed: usize,
    skipped_already_nfc: usize,
    collisions: usize,
    errors: usize,
}

fn cmd_rename(args: RenameArgs) -> Result<()> {
    let mut stats = RenameStats::default();
    for root in &args.paths {
        process_rename_root(root, &args, &mut stats);
    }
    eprintln!(
        "\nscanned: {}, renamed: {}, already NFC: {}, collisions: {}, errors: {}{}",
        stats.scanned,
        stats.renamed,
        stats.skipped_already_nfc,
        stats.collisions,
        stats.errors,
        if args.dry_run { "  (dry-run)" } else { "" }
    );
    if stats.errors > 0 || stats.collisions > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn process_rename_root(root: &Path, args: &RenameArgs, stats: &mut RenameStats) {
    let meta = match std::fs::symlink_metadata(root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("stat {}: {e}", root.display());
            stats.errors += 1;
            return;
        }
    };
    if !meta.is_dir() {
        stats.scanned += 1;
        if let Err(e) = rename_if_needed(root, args, stats) {
            eprintln!("error for {}: {e:#}", root.display());
            stats.errors += 1;
        }
        return;
    }

    let walker = WalkDir::new(root)
        .min_depth(0)
        .max_depth(if args.recursive { usize::MAX } else { 1 })
        .contents_first(true);

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("walk error: {e}");
                stats.errors += 1;
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        stats.scanned += 1;
        if let Err(e) = rename_if_needed(entry.path(), args, stats) {
            eprintln!("error for {}: {e:#}", entry.path().display());
            stats.errors += 1;
        }
    }
}

fn rename_if_needed(path: &Path, args: &RenameArgs, stats: &mut RenameStats) -> Result<()> {
    let file_name = match path.file_name().and_then(|s| s.to_str()) {
        Some(s) => s,
        None => return Ok(()),
    };
    if is_nfc_quick(file_name.chars()) == IsNormalized::Yes {
        stats.skipped_already_nfc += 1;
        return Ok(());
    }
    let nfc: String = file_name.nfc().collect();
    if nfc == file_name {
        stats.skipped_already_nfc += 1;
        return Ok(());
    }
    let parent = path.parent().context("no parent directory")?;
    let target = parent.join(&nfc);
    if target.exists() {
        let src_meta = std::fs::symlink_metadata(path)
            .with_context(|| format!("stat {}", path.display()))?;
        let tgt_meta = std::fs::symlink_metadata(&target)
            .with_context(|| format!("stat {}", target.display()))?;
        let same = src_meta.dev() == tgt_meta.dev() && src_meta.ino() == tgt_meta.ino();
        if !same {
            eprintln!(
                "collision: {} -> {} (target exists)",
                path.display(),
                target.display()
            );
            stats.collisions += 1;
            return Ok(());
        }
    }
    if !args.quiet {
        println!("{}  ->  {}", path.display(), nfc);
    }
    if !args.dry_run {
        std::fs::rename(path, &target)
            .with_context(|| format!("rename {} -> {}", path.display(), target.display()))?;
    }
    stats.renamed += 1;
    Ok(())
}

// -------- zip --------

fn cmd_zip(args: ZipArgs) -> Result<()> {
    let folder = args
        .folder
        .canonicalize()
        .with_context(|| format!("folder not found: {}", args.folder.display()))?;
    let output = match &args.output {
        Some(p) => p.clone(),
        None => default_output_for(&folder)?,
    };
    let options = ZipOptions {
        level: args.level,
        include_mac_cruft: args.include_mac_cruft,
        wrap: !args.no_wrap,
    };
    let quiet = args.quiet;
    let stats = write_zip(&folder, &output, &options, |name, is_dir, converted| {
        if !quiet {
            let marker = if converted { "*" } else { " " };
            let kind = if is_dir { "D" } else { "F" };
            println!("{marker} {kind}  {name}");
        }
    })?;
    eprintln!(
        "\nwrote {} — files: {}, dirs: {}, skipped: {}, uncompressed: {} bytes",
        output.display(),
        stats.files,
        stats.dirs,
        stats.skipped,
        stats.bytes
    );
    Ok(())
}
