use hangul_conv::{ZipOptions, default_output_for, rename_tree, write_zip};
use serde::Serialize;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};

#[derive(Serialize, Clone)]
struct RenameEntry {
    old_path: String,
    new_name: String,
}

#[derive(Serialize, Clone)]
struct ZipEntry {
    name: String,
    is_dir: bool,
    converted: bool,
}

#[derive(Serialize)]
struct ProcessResult {
    output: Option<String>,
    files: usize,
    dirs: usize,
    skipped: usize,
    converted: usize,
    bytes: u64,
    renamed_on_disk: usize,
    rename_collisions: usize,
}

#[tauri::command]
async fn process_folder(
    app: AppHandle,
    path: String,
    make_zip: bool,
) -> Result<ProcessResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let folder = PathBuf::from(&path)
            .canonicalize()
            .map_err(|e| format!("folder not found: {path}: {e}"))?;
        if !folder.is_dir() {
            return Err(format!("not a directory: {}", folder.display()));
        }

        // 1) Rename originals in place (NFD → NFC).
        let rename_emitter = app.clone();
        let rename_stats = rename_tree(&folder, |old_path, new_name| {
            let _ = rename_emitter.emit(
                "rename:entry",
                RenameEntry {
                    old_path: old_path.display().to_string(),
                    new_name: new_name.to_string(),
                },
            );
        })
        .map_err(|e| format!("rename: {e:#}"))?;

        // 2) Write the zip (optional).
        let (output_str, zip_stats) = if make_zip {
            let output = default_output_for(&folder).map_err(|e| e.to_string())?;
            if output.exists() {
                return Err(format!("output already exists: {}", output.display()));
            }
            let options = ZipOptions::default();
            let zip_emitter = app.clone();
            let stats = write_zip(&folder, &output, &options, |name, is_dir, converted| {
                let _ = zip_emitter.emit(
                    "zip:entry",
                    ZipEntry {
                        name: name.to_string(),
                        is_dir,
                        converted,
                    },
                );
            })
            .map_err(|e| format!("zip: {e:#}"))?;
            (Some(output.display().to_string()), stats)
        } else {
            (None, Default::default())
        };

        Ok(ProcessResult {
            output: output_str,
            files: zip_stats.files,
            dirs: zip_stats.dirs,
            skipped: zip_stats.skipped,
            converted: zip_stats.converted,
            bytes: zip_stats.bytes,
            renamed_on_disk: rename_stats.renamed,
            rename_collisions: rename_stats.collisions,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![process_folder])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
