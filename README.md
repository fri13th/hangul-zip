# hangul-zip

Fix Korean filenames when moving from macOS (NFD) to Windows (NFC), with a CLI and a drag-and-drop GUI that produces Windows-compatible zips.

## The problem

macOS stores Korean filenames as decomposed jamo (Unicode NFD) — `부` is stored as `ㅂ + ㅜ`. Windows, Linux, and most archive tools expect precomposed syllables (NFC). The mismatch shows up as broken Korean text on Windows when you send files across, and macOS's built-in Archive Utility bakes NFD names into zips — so even "Compress" on Finder gives you unreadable filenames on the other side.

This tool fixes both problems: rename originals in place to NFC, and build zips with NFC entry names and the UTF-8 flag set correctly.

## What's included

- `hangul-conv` — CLI with two subcommands: `rename` (in place) and `zip` (Windows-compatible archive)
- `Hangul Zip.app` — macOS Tauri app: drag a folder in, get NFC-renamed originals and (optionally) a Windows-compatible zip next to it

## Build

Requires Rust (stable) and, for the GUI, `cargo-tauri`:

```sh
# CLI only
cargo build --release
./target/release/hangul-conv --help

# GUI: .app + .dmg bundles
cargo install tauri-cli --version "^2.0" --locked
cargo tauri build
# artifacts in src-tauri/target/release/bundle/
```

## CLI usage

```sh
# rename NFD → NFC in place, recursive, with dry-run
hangul-conv rename -r -n /path/to/folder
hangul-conv rename -r     /path/to/folder

# Windows-compatible zip (NFC entry names, UTF-8 flag, wraps contents
# under a top-level folder named after the source)
hangul-conv zip /path/to/folder

# skip the wrap
hangul-conv zip --no-wrap /path/to/folder

# keep macOS cruft (.DS_Store, ._*, __MACOSX/) in the archive
hangul-conv zip --include-mac-cruft /path/to/folder
```

## GUI usage

1. Drop the `.app` into `/Applications` (first launch: right-click → Open to bypass Gatekeeper — the binary is unsigned).
2. Drag one or more folders onto the window.
3. Pick a mode:
   - **Rename only** — rename children in place, nothing else.
   - **Rename + zip** (default) — rename in place, then produce `<folder>.zip` next to the source.
4. The log clears on each drop and shows per-entry progress: renamed files, zipped entries (with `zipped*` marking entries whose basename was NFC-converted), and a summary line at the end.

## How it works

Two operations. Both walk the tree with `walkdir`.

**Rename** walks deepest-first (`contents_first`) so that renaming a directory doesn't invalidate child paths still to visit. On macOS, filesystem lookups are normalization-insensitive: `target.exists()` returns true for an NFC target path even when only the NFD source exists, because the filesystem matches both forms to the same directory entry. The tool handles this by comparing `dev`+`inode` of source and target — same inode means same file (not a collision), so the rename proceeds.

**Zip** writes NFC-normalized names per path component, sets the UTF-8 general-purpose flag (bit 11) on every entry so Windows Explorer and Python's `zipfile` handle Korean correctly, skips `.DS_Store`/`__MACOSX`/`._*` AppleDouble cruft by default, and wraps contents under a top-level folder named after the source (matching Finder's "Compress" default).

## License

MIT
