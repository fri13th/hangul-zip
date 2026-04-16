use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick};
use walkdir::WalkDir;
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

#[derive(Default, Debug, Clone)]
pub struct RenameStats {
    pub scanned: usize,
    pub renamed: usize,
    pub skipped_already_nfc: usize,
    pub collisions: usize,
    pub errors: usize,
}

/// Recursively rename files and directories under `root` from NFD to NFC.
/// Walks deepest-first so that renaming a directory doesn't invalidate
/// paths we still need to visit. The `root` itself is not renamed.
///
/// `on_rename(old_path, new_name)` is called for each successful rename.
pub fn rename_tree(
    root: &Path,
    mut on_rename: impl FnMut(&Path, &str),
) -> Result<RenameStats> {
    anyhow::ensure!(root.is_dir(), "not a directory: {}", root.display());
    let mut stats = RenameStats::default();
    let walker = WalkDir::new(root).min_depth(1).contents_first(true);

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        stats.scanned += 1;
        let path = entry.path();
        let file_name = match path.file_name().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        if is_nfc_quick(file_name.chars()) == IsNormalized::Yes {
            stats.skipped_already_nfc += 1;
            continue;
        }
        let nfc: String = file_name.nfc().collect();
        if nfc == file_name {
            stats.skipped_already_nfc += 1;
            continue;
        }
        let parent = path.parent().context("no parent")?;
        let target = parent.join(&nfc);

        // macOS filesystem lookups are normalization-insensitive, so
        // target.exists() is always true for an NFD source. Only treat
        // it as a collision if the target has a different inode.
        if target.exists() {
            let src_meta = std::fs::symlink_metadata(path)?;
            let tgt_meta = std::fs::symlink_metadata(&target)?;
            let same = src_meta.dev() == tgt_meta.dev() && src_meta.ino() == tgt_meta.ino();
            if !same {
                stats.collisions += 1;
                continue;
            }
        }

        std::fs::rename(path, &target)
            .with_context(|| format!("rename {} -> {}", path.display(), target.display()))?;
        stats.renamed += 1;
        on_rename(path, &nfc);
    }
    Ok(stats)
}

#[derive(Debug, Clone)]
pub struct ZipOptions {
    pub level: u8,
    pub include_mac_cruft: bool,
    pub wrap: bool,
}

impl Default for ZipOptions {
    fn default() -> Self {
        Self {
            level: 6,
            include_mac_cruft: false,
            wrap: true,
        }
    }
}

#[derive(Default, Debug, Clone)]
pub struct ZipStats {
    pub files: usize,
    pub dirs: usize,
    pub skipped: usize,
    pub converted: usize,
    pub bytes: u64,
}

/// Compute the default zip output path: `<folder-name>.zip` next to the
/// source folder, with the folder name NFC-normalized.
pub fn default_output_for(folder: &Path) -> Result<PathBuf> {
    let name = folder
        .file_name()
        .context("folder has no name")?
        .to_string_lossy()
        .nfc()
        .collect::<String>();
    let parent = folder.parent().context("folder has no parent")?;
    Ok(parent.join(format!("{name}.zip")))
}

pub fn write_zip(
    folder: &Path,
    output: &Path,
    options: &ZipOptions,
    mut on_entry: impl FnMut(&str, bool, bool),
) -> Result<ZipStats> {
    anyhow::ensure!(folder.is_dir(), "not a directory: {}", folder.display());
    anyhow::ensure!(!output.exists(), "output already exists: {}", output.display());

    let mut stats = ZipStats::default();
    let file = File::create(output)
        .with_context(|| format!("create {}", output.display()))?;
    let mut writer = ZipWriter::new(BufWriter::new(file));

    let method = if options.level == 0 {
        CompressionMethod::Stored
    } else {
        CompressionMethod::Deflated
    };
    let file_options: SimpleFileOptions = SimpleFileOptions::default()
        .compression_method(method)
        .compression_level(Some(options.level as i64))
        .unix_permissions(0o644);

    let wrap_prefix: Option<String> = if options.wrap {
        Some(
            folder
                .file_name()
                .context("folder has no name")?
                .to_string_lossy()
                .nfc()
                .collect::<String>(),
        )
    } else {
        None
    };

    let walker = WalkDir::new(folder).min_depth(1).sort_by_file_name();
    for entry in walker {
        let entry = entry.context("walk error")?;
        let path = entry.path();
        let rel = path.strip_prefix(folder).context("strip_prefix failed")?;

        if !options.include_mac_cruft && is_mac_cruft(rel) {
            stats.skipped += 1;
            continue;
        }

        let mut parts: Vec<String> = Vec::new();
        if let Some(ref w) = wrap_prefix {
            parts.push(w.clone());
        }
        let mut last_converted = false;
        for c in rel.components() {
            let orig = c.as_os_str().to_string_lossy();
            let nfc: String = orig.nfc().collect();
            last_converted = nfc != orig;
            parts.push(nfc);
        }
        let converted = last_converted;
        let archive_name = parts.join("/");

        let ft = entry.file_type();
        if ft.is_dir() {
            let dir_name = format!("{archive_name}/");
            writer
                .add_directory(&dir_name, file_options)
                .with_context(|| format!("add dir {dir_name}"))?;
            stats.dirs += 1;
            if converted {
                stats.converted += 1;
            }
            on_entry(&dir_name, true, converted);
        } else if ft.is_file() {
            writer
                .start_file(&archive_name, file_options)
                .with_context(|| format!("start_file {archive_name}"))?;
            let mut src = BufReader::new(
                File::open(path).with_context(|| format!("open {}", path.display()))?,
            );
            let n = std::io::copy(&mut src, &mut writer)
                .with_context(|| format!("copy {}", path.display()))?;
            stats.files += 1;
            stats.bytes += n;
            if converted {
                stats.converted += 1;
            }
            on_entry(&archive_name, false, converted);
        } else {
            stats.skipped += 1;
        }
    }

    writer.finish().context("finalize zip")?;
    Ok(stats)
}

fn is_mac_cruft(rel: &Path) -> bool {
    for comp in rel.components() {
        let s = comp.as_os_str().to_string_lossy();
        if s == ".DS_Store" || s == "__MACOSX" || s == "Thumbs.db" {
            return true;
        }
        if s.starts_with("._") {
            return true;
        }
    }
    false
}
