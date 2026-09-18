mod archive;
mod cloud;
mod crypto;
mod domain;
mod media;
mod mobile;
mod progress;
mod provider;
mod repository;
mod secrets;
mod telegram;
mod transfer;
mod upload_advisor;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use cloud::BatchDownloadItem;
use crypto::{decrypt_file, sha256_file};
use domain::{AppSettings, DashboardData, DashboardStatus};
use media::{
    clear_media_cache, reconcile_media_cache_index, remove_media_cache_entries, MediaReady,
};
use provider::StorageProvider;
use repository::CatalogRepository;
use tauri::{Emitter, Manager, State};
use telegram::{
    TelegramAuthSnapshot, TelegramLoginEmailCodeInfo, TelegramLoginEmailStatus, TelegramService,
};
use transfer::{PreparedUpload, TransferService};
use zeroize::Zeroize;

struct DynamicLimiter {
    max: usize,
    limit: AtomicUsize,
    active: AtomicUsize,
    notify: tokio::sync::Notify,
}

impl DynamicLimiter {
    fn new(limit: usize, max: usize) -> Arc<Self> {
        Arc::new(Self {
            max,
            limit: AtomicUsize::new(limit.clamp(1, max)),
            active: AtomicUsize::new(0),
            notify: tokio::sync::Notify::new(),
        })
    }

    fn set_limit(&self, limit: usize) {
        self.limit
            .store(limit.clamp(1, self.max), Ordering::Release);
        self.notify.notify_waiters();
    }

    fn try_acquire(self: &Arc<Self>) -> Option<DynamicPermit> {
        loop {
            let active = self.active.load(Ordering::Acquire);
            let limit = self.limit.load(Ordering::Acquire);
            if active >= limit {
                return None;
            }
            if self
                .active
                .compare_exchange_weak(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(DynamicPermit {
                    limiter: self.clone(),
                });
            }
        }
    }

    async fn acquire(self: &Arc<Self>) -> DynamicPermit {
        loop {
            if let Some(permit) = self.try_acquire() {
                return permit;
            }
            // Notify is edge-triggered. Keep a tiny timeout as a safety net so a
            // permit released between try_acquire() and notified().await cannot
            // leave a preparation stalled forever.
            tokio::select! {
                _ = self.notify.notified() => {},
                _ = tokio::time::sleep(Duration::from_millis(100)) => {},
            }
        }
    }
}

struct DynamicPermit {
    limiter: Arc<DynamicLimiter>,
}

impl Drop for DynamicPermit {
    fn drop(&mut self) {
        self.limiter.active.fetch_sub(1, Ordering::AcqRel);
        self.limiter.notify.notify_one();
    }
}

struct AppState {
    repository: CatalogRepository,
    telegram: TelegramService,
    staging_dir: PathBuf,
    media_cache_dir: PathBuf,
    preparation_slots: Arc<DynamicLimiter>,
    upload_slots: Arc<DynamicLimiter>,
    download_slots: Arc<DynamicLimiter>,
    worker_wake: Arc<tokio::sync::Notify>,
    sync_lock: tokio::sync::Mutex<()>,
    background_error: Mutex<Option<String>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncDelta {
    cursor: i64,
    sync_progress: progress::SyncProgress,
    files: Vec<domain::CloudFile>,
    removed_ids: Vec<String>,
    folders: Option<Vec<domain::CloudFolder>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadedImageCleanupSummary {
    count: usize,
    bytes: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadedImageCleanupResult {
    deleted: usize,
    released_bytes: i64,
    skipped: usize,
    failed: usize,
}

#[cfg(target_os = "android")]
fn cleanup_android_staged_source(path: &Path) {
    let _ = fs::remove_file(path);
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir(parent);
    }
}

#[tauri::command]
async fn get_dashboard(state: State<'_, Arc<AppState>>) -> Result<DashboardData, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || dashboard_snapshot(&state))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_dashboard_status(
    state: State<'_, Arc<AppState>>,
    after_history_cursor: Option<i64>,
) -> Result<DashboardStatus, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        dashboard_status_snapshot(&state, after_history_cursor)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[allow(clippy::too_many_arguments)] // Tauri maps these flat fields directly from the JS invoke payload.
#[tauri::command]
async fn get_catalog_page(
    state: State<'_, Arc<AppState>>,
    section: String,
    folder_id: Option<String>,
    kind: Option<String>,
    search: Option<String>,
    tag: Option<String>,
    sort: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<domain::CatalogPage, String> {
    if !matches!(
        section.as_str(),
        "home" | "files" | "favorites" | "recent" | "trash"
    ) {
        return Err("Sección de catálogo inválida".into());
    }
    let sort = sort.unwrap_or_else(|| "recent".to_string());
    if !matches!(sort.as_str(), "recent" | "oldest" | "name" | "size") {
        return Err("Orden de catálogo inválido".into());
    }
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state
            .repository
            .list_files_page(repository::CatalogPageQuery {
                section,
                folder_id,
                kind,
                search: search.unwrap_or_default(),
                tag,
                sort,
                offset: offset.unwrap_or(0),
                limit: limit.unwrap_or(80),
            })
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_sync_delta(
    state: State<'_, Arc<AppState>>,
    after_cursor: Option<i64>,
) -> Result<SyncDelta, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let progress = state
            .telegram
            .sync_progress
            .lock()
            .map_err(|e| e.to_string())?
            .clone();
        let delta = match after_cursor {
            Some(after) => state
                .repository
                .sync_file_delta(after)
                .map_err(|e| e.to_string())?,
            None => repository::CatalogDelta {
                cursor: state
                    .repository
                    .catalog_change_cursor()
                    .map_err(|e| e.to_string())?,
                files: Vec::new(),
                removed_ids: Vec::new(),
                folders_changed: true,
            },
        };
        let folders = if after_cursor.is_none()
            || delta.folders_changed
            || progress.phase == crate::progress::SyncPhase::Folders
        {
            Some(state.repository.list_folders().map_err(|e| e.to_string())?)
        } else {
            None
        };
        Ok(SyncDelta {
            cursor: delta.cursor,
            sync_progress: progress,
            files: delta.files,
            removed_ids: delta.removed_ids,
            folders,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn dashboard_status_snapshot(
    state: &AppState,
    after_history_cursor: Option<i64>,
) -> Result<DashboardStatus, String> {
    let transfers = state
        .repository
        .list_active_transfers()
        .map_err(|error| error.to_string())?;
    let history_cursor = state
        .repository
        .transfer_history_cursor()
        .map_err(|error| error.to_string())?;
    let transfer_history = if after_history_cursor == Some(history_cursor) {
        None
    } else {
        Some(
            state
                .repository
                .list_transfer_history(1000)
                .map_err(|error| error.to_string())?,
        )
    };
    let telegram_status = state.telegram.status();
    let settings = state.repository.settings().map_err(|e| e.to_string())?;
    let cache_bytes = state
        .repository
        .media_cache_total_bytes()
        .map_err(|e| e.to_string())?;
    let queue_summary = state
        .repository
        .queue_summary(cache_bytes)
        .map_err(|e| e.to_string())?;
    let (total_bytes, file_count, favorite_count, trash_count) = state
        .repository
        .catalog_stats()
        .map_err(|e| e.to_string())?;
    let catalog_cursor = state
        .repository
        .catalog_change_cursor()
        .map_err(|e| e.to_string())?;

    Ok(DashboardStatus {
        sync_progress: state
            .telegram
            .sync_progress
            .lock()
            .expect("sync progress")
            .clone(),
        transfers,
        transfer_history,
        total_bytes,
        file_count,
        favorite_count,
        trash_count,
        recent_count: file_count,
        telegram_connected: telegram_status.connected,
        telegram_account_label: telegram_status.account_label,
        provider_status: state
            .background_error
            .lock()
            .expect("background")
            .clone()
            .unwrap_or_else(|| {
                format!(
                    "{} · {}",
                    telegram_status.label,
                    state.telegram.provider_id()
                )
            }),
        queue_summary,
        settings,
        is_premium: state.telegram.cached_snapshot().is_premium,
        catalog_cursor,
        history_cursor,
    })
}

fn dashboard_snapshot(state: &AppState) -> Result<DashboardData, String> {
    let folders = state
        .repository
        .list_folders()
        .map_err(|error| error.to_string())?;
    let status = dashboard_status_snapshot(state, None)?;

    Ok(DashboardData {
        sync_progress: status.sync_progress,
        files: Vec::new(),
        folders,
        transfers: status.transfers,
        transfer_history: status.transfer_history.unwrap_or_default(),
        total_bytes: status.total_bytes,
        file_count: status.file_count,
        favorite_count: status.favorite_count,
        trash_count: status.trash_count,
        recent_count: status.recent_count,
        telegram_connected: status.telegram_connected,
        telegram_account_label: status.telegram_account_label,
        provider_status: status.provider_status,
        queue_summary: status.queue_summary,
        settings: status.settings,
        is_premium: status.is_premium,
        catalog_cursor: status.catalog_cursor,
        history_cursor: status.history_cursor,
    })
}

#[tauri::command]
fn set_favorite(
    state: State<'_, Arc<AppState>>,
    id: String,
    favorite: bool,
) -> Result<bool, String> {
    state
        .repository
        .set_favorite(&id, favorite)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn analyze_upload_selection(
    state: State<'_, Arc<AppState>>,
    items: Vec<upload_advisor::UploadAdvisoryItem>,
) -> Result<upload_advisor::UploadAdvisory, String> {
    let provider_limit_bytes = state.telegram.max_upload_bytes();
    tauri::async_runtime::spawn_blocking(move || {
        upload_advisor::analyze_upload_selection(&items, provider_limit_bytes)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn prepare_zip_uploads(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    mut items: Vec<archive::ArchiveInput>,
    folder_id: Option<String>,
) -> Result<Vec<Result<PreparedUpload, String>>, String> {
    if items.is_empty() || items.len() > 10000 {
        return Err("Selecciona entre 1 y 10000 archivos".into());
    }
    if let Some(id) = folder_id.as_deref() {
        let folder = state
            .repository
            .folder_by_id(id)
            .map_err(|e| e.to_string())?;
        if folder.trashed {
            return Err("La carpeta está en la papelera".into());
        }
    }
    state.telegram.own_chat(&state.repository).await?;

    #[cfg(target_os = "android")]
    let mut staged_android_sources = Vec::new();
    #[cfg(target_os = "android")]
    for item in &mut items {
        if item.path.starts_with("content://") {
            match mobile::stage_content_uri(&item.path).await {
                Ok(staged) => {
                    staged_android_sources.push(PathBuf::from(&staged.path));
                    item.path = staged.path;
                }
                Err(error) => {
                    for staged in &staged_android_sources {
                        cleanup_android_staged_source(staged);
                    }
                    return Err(error);
                }
            }
        }
    }
    #[cfg(not(target_os = "android"))]
    let _ = &mut items;

    let state = state.inner().clone();
    let permit = state.preparation_slots.acquire().await;
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        let temp = tempfile::Builder::new()
            .prefix("zip-")
            .tempdir_in(&state.staging_dir)
            .map_err(|e| e.to_string())?;
        let paths =
            archive::create_archives(&items, temp.path(), archive::ZIP_LIMIT, |progress| {
                let _ = app.emit("nuvio-zip-progress", progress);
            })?;
        let results: Vec<_> = paths
            .iter()
            .map(|path| {
                TransferService::adopt_generated_upload_in_folder(
                    &state.repository,
                    &path.to_string_lossy(),
                    &state.staging_dir,
                    folder_id.as_deref(),
                )
            })
            .collect();
        // If one generated archive could not be adopted, preserve the temporary
        // source so it is not silently discarded while reporting the failure.
        if results.iter().any(Result::is_err) {
            let _ = temp.keep();
        }
        if results.iter().any(Result::is_ok) {
            state.worker_wake.notify_one();
        }
        Ok(results)
    })
    .await
    .map_err(|e| e.to_string())?;

    #[cfg(target_os = "android")]
    for staged in &staged_android_sources {
        cleanup_android_staged_source(staged);
    }

    result
}

#[tauri::command]
async fn prepare_upload(
    state: State<'_, Arc<AppState>>,
    #[allow(unused_mut)] mut path: String,
    encrypt: bool,
    passphrase: Option<String>,
    folder_id: Option<String>,
) -> Result<PreparedUpload, String> {
    let original_source = path.clone();
    let state = state.inner().clone();
    let delete_source_after_upload = state
        .repository
        .settings()
        .map_err(|e| e.to_string())?
        .delete_original_after_upload;
    if let Some(folder) = folder_id.as_deref() {
        let target = state
            .repository
            .folder_by_id(folder)
            .map_err(|e| e.to_string())?;
        if target.trashed {
            return Err("No puedes subir archivos a una carpeta eliminada".into());
        }
    }
    let _permit = state.preparation_slots.acquire().await;
    state.telegram.own_chat(&state.repository).await?;
    if encrypt {
        return Err("El cifrado adicional de archivos todavía no está habilitado para transferencias Telegram".into());
    }

    #[cfg(target_os = "android")]
    if path.starts_with("content://") {
        let staged = mobile::stage_content_uri(&path).await?;
        let staged_path = PathBuf::from(&staged.path);
        let result: Result<PreparedUpload, String> = async {
            if staged.size_bytes <= 0 {
                return Err("Telegram no permite subir archivos vacíos (0 B)".into());
            }
            let limit = state.telegram.max_upload_bytes();
            if staged.size_bytes as u64 > limit {
                let size_gb = staged.size_bytes as f64 / 1_000_000_000.0;
                return Err(format!(
                    "Pesa {:.2} GB. Supera el límite de {} GB por archivo de Telegram ({})",
                    size_gb,
                    limit / 1_000_000_000,
                    if state.telegram.cached_snapshot().is_premium {
                        "límite Telegram Premium"
                    } else {
                        "cuenta estándar"
                    }
                ));
            }
            let owned = state.clone();
            let original_uri = path.clone();
            let staged_path_text = staged.path.clone();
            let size_bytes = staged.size_bytes;
            let sha256 = staged.sha256.clone();
            tauri::async_runtime::spawn_blocking(move || {
                TransferService::adopt_preverified_upload_in_folder(
                    &owned.repository,
                    &staged_path_text,
                    &original_uri,
                    size_bytes,
                    &sha256,
                    &owned.staging_dir,
                    folder_id.as_deref(),
                    delete_source_after_upload,
                )
            })
            .await
            .map_err(|e| e.to_string())?
        }
        .await;
        cleanup_android_staged_source(&staged_path);
        if result.is_ok() {
            state.worker_wake.notify_one();
        }
        return result;
    }

    let result: Result<PreparedUpload, String> = async {
        let limit = state.telegram.max_upload_bytes();
        let source = transfer::normalize_path(&path)?;
        let metadata = fs::metadata(&source).map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            return Err("La selección no es un archivo".into());
        }
        let size = metadata.len();
        if size == 0 {
            return Err("Telegram no permite subir archivos vacíos (0 B)".into());
        }
        if size > limit {
            let size_gb = size as f64 / 1_000_000_000.0;
            return Err(format!(
                "Pesa {:.2} GB. Supera el límite de {} GB por archivo de Telegram ({})",
                size_gb,
                limit / 1_000_000_000,
                if state.telegram.cached_snapshot().is_premium {
                    "límite Telegram Premium"
                } else {
                    "cuenta estándar"
                }
            ));
        }
        let owned = state.clone();
        let prepared_path = path.clone();
        tauri::async_runtime::spawn_blocking(move || {
            TransferService::prepare_upload_in_folder(
                &owned.repository,
                &prepared_path,
                Some(&original_source),
                false,
                passphrase,
                &owned.staging_dir,
                folder_id.as_deref(),
                delete_source_after_upload,
            )
        })
        .await
        .map_err(|e| e.to_string())?
    }
    .await;

    if result.is_ok() {
        state.worker_wake.notify_one();
    }
    result
}

#[tauri::command]
async fn sync_files(state: State<'_, Arc<AppState>>) -> Result<usize, String> {
    let _guard = state
        .sync_lock
        .try_lock()
        .map_err(|_| "Ya hay una sincronización en curso")?;
    let result = state
        .telegram
        .sync_catalog_checked(&state.repository, true)
        .await;
    *state.background_error.lock().expect("background") = result.as_ref().err().cloned();
    if result.is_ok() {
        let _ = state.repository.optimize();
    }
    result
}

#[tauri::command]
async fn create_folder(
    state: State<'_, Arc<AppState>>,
    name: String,
    parent_id: Option<String>,
) -> Result<String, String> {
    let _guard = state.sync_lock.lock().await;
    state
        .telegram
        .create_folder_synced(&state.repository, &name, parent_id.as_deref())
        .await
}

#[tauri::command]
async fn rename_folder(
    state: State<'_, Arc<AppState>>,
    id: String,
    name: String,
) -> Result<(), String> {
    let _guard = state.sync_lock.lock().await;
    let folder = state
        .repository
        .folder_by_id(&id)
        .map_err(|e| e.to_string())?;
    state
        .telegram
        .update_folder_synced(
            &state.repository,
            &id,
            &name,
            folder.parent_id.as_deref(),
            folder.trashed,
        )
        .await
}

#[tauri::command]
async fn move_folder(
    state: State<'_, Arc<AppState>>,
    id: String,
    parent_id: Option<String>,
) -> Result<(), String> {
    let _guard = state.sync_lock.lock().await;
    let folder = state
        .repository
        .folder_by_id(&id)
        .map_err(|e| e.to_string())?;
    state
        .telegram
        .update_folder_synced(
            &state.repository,
            &id,
            &folder.name,
            parent_id.as_deref(),
            folder.trashed,
        )
        .await
}

#[tauri::command]
async fn delete_folder(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    let _guard = state.sync_lock.lock().await;
    let folder = state
        .repository
        .folder_by_id(&id)
        .map_err(|e| e.to_string())?;
    state
        .telegram
        .update_folder_synced(
            &state.repository,
            &id,
            &folder.name,
            folder.parent_id.as_deref(),
            true,
        )
        .await
}

#[tauri::command]
async fn move_files_to_folder(
    state: State<'_, Arc<AppState>>,
    ids: Vec<String>,
    folder_id: Option<String>,
) -> Result<usize, String> {
    let _guard = state.sync_lock.lock().await;
    state
        .telegram
        .move_files_synced(&state.repository, &ids, folder_id.as_deref())
        .await
}

#[tauri::command]
async fn pick_download_directory() -> Result<Option<String>, String> {
    #[cfg(target_os = "android")]
    return mobile::pick_directory().await;
    #[cfg(not(target_os = "android"))]
    Ok(None)
}

const MAX_DIRECTORY_UPLOAD_FILES: usize = 10_000;
const MAX_DIRECTORY_UPLOAD_FOLDERS: usize = 10_000;
const MAX_DIRECTORY_UPLOAD_DEPTH: usize = 128;

fn validate_directory_upload_limits(
    file_count: usize,
    folder_count: usize,
    depth: usize,
) -> Result<(), String> {
    if file_count > MAX_DIRECTORY_UPLOAD_FILES {
        return Err("Selecciona una carpeta con máximo 10000 archivos por operación".into());
    }
    if folder_count > MAX_DIRECTORY_UPLOAD_FOLDERS {
        return Err(
            "La carpeta contiene demasiadas subcarpetas para procesarla de forma segura".into(),
        );
    }
    if depth > MAX_DIRECTORY_UPLOAD_DEPTH {
        return Err(
            "La carpeta seleccionada supera la profundidad máxima admitida por Nuvio".into(),
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScannedUploadFile {
    pub relative_path: String,
    pub absolute_path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryUploadPlan {
    pub root_name: String,
    pub folders: Vec<String>,
    pub files: Vec<ScannedUploadFile>,
    pub total_bytes: u64,
}

pub fn build_directory_upload_plan(dir_path: &Path) -> Result<DirectoryUploadPlan, String> {
    if !dir_path.exists() {
        return Err("La carpeta seleccionada no existe".to_string());
    }
    if !dir_path.is_dir() {
        return Err("La ruta seleccionada no es una carpeta".to_string());
    }
    let root_name = dir_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Carpeta".to_string());

    let mut folders_set = std::collections::BTreeSet::new();
    let mut files = Vec::new();
    let mut total_bytes = 0u64;

    fn walk_dir(
        current: &Path,
        root: &Path,
        depth: usize,
        folders_set: &mut std::collections::BTreeSet<String>,
        files: &mut Vec<ScannedUploadFile>,
        total_bytes: &mut u64,
    ) -> Result<(), String> {
        validate_directory_upload_limits(files.len(), folders_set.len(), depth)?;
        let entries =
            std::fs::read_dir(current).map_err(|e| format!("No se pudo leer la carpeta: {e}"))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("Error al leer elemento: {e}"))?;
            let path = entry.path();
            let file_name = entry.file_name();
            let file_name_str = file_name.to_string_lossy();

            if file_name_str.starts_with('.')
                || file_name_str.eq_ignore_ascii_case("thumbs.db")
                || file_name_str.eq_ignore_ascii_case("desktop.ini")
            {
                continue;
            }

            let rel_path = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");

            let file_type = entry.file_type().map_err(|e| e.to_string())?;
            if file_type.is_symlink() {
                continue;
            }
            let metadata = entry.metadata().map_err(|e| e.to_string())?;
            if metadata.is_dir() {
                folders_set.insert(rel_path.clone());
                validate_directory_upload_limits(files.len(), folders_set.len(), depth + 1)?;
                walk_dir(&path, root, depth + 1, folders_set, files, total_bytes)?;
            } else if metadata.is_file() {
                validate_directory_upload_limits(files.len() + 1, folders_set.len(), depth)?;
                let size = metadata.len();
                *total_bytes = total_bytes
                    .checked_add(size)
                    .ok_or("La carpeta seleccionada es demasiado grande para contabilizarla")?;
                if let Some(parent) = path.parent() {
                    if let Ok(parent_rel) = parent.strip_prefix(root) {
                        let normalized_parent = parent_rel.to_string_lossy().replace('\\', "/");
                        if !normalized_parent.is_empty() {
                            folders_set.insert(normalized_parent);
                        }
                    }
                }
                files.push(ScannedUploadFile {
                    relative_path: rel_path,
                    absolute_path: path.to_string_lossy().to_string(),
                    size,
                });
            }
        }
        Ok(())
    }

    walk_dir(
        dir_path,
        dir_path,
        0,
        &mut folders_set,
        &mut files,
        &mut total_bytes,
    )?;

    let mut folders: Vec<String> = folders_set.into_iter().collect();
    folders.sort_by(|a, b| {
        let depth_a = a.split('/').count();
        let depth_b = b.split('/').count();
        depth_a.cmp(&depth_b).then_with(|| a.cmp(b))
    });

    Ok(DirectoryUploadPlan {
        root_name,
        folders,
        files,
        total_bytes,
    })
}

#[tauri::command]
async fn scan_directory_for_upload(path: String) -> Result<DirectoryUploadPlan, String> {
    let target = Path::new(&path);
    build_directory_upload_plan(target)
}

#[tauri::command]
async fn pick_upload_directory() -> Result<Option<DirectoryUploadPlan>, String> {
    mobile::pick_upload_directory().await
}

#[tauri::command]
async fn pick_upload_files() -> Result<Vec<String>, String> {
    mobile::pick_upload_files().await
}

#[tauri::command]
fn platform_name() -> &'static str {
    std::env::consts::OS
}

#[tauri::command]
fn background_app() -> Result<(), String> {
    mobile::background()
}

#[tauri::command]
fn queue_downloads(
    state: State<'_, Arc<AppState>>,
    ids: Vec<String>,
    directory: String,
    conflict_policy: Option<String>,
) -> Result<Vec<BatchDownloadItem>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    if ids.len() > 500 {
        return Err("Selecciona como máximo 500 archivos por lote".into());
    }
    let settings = state.repository.settings().map_err(|e| e.to_string())?;
    let policy = conflict_policy.unwrap_or(settings.conflict_policy);
    if policy != "skip" && policy != "rename" {
        return Err("Política de archivos existentes no válida".into());
    }
    #[cfg(target_os = "android")]
    if directory.starts_with("content://") {
        let names = mobile::directory_names(&directory)?;
        let queued = state
            .repository
            .enqueue_android_downloads(&ids, &directory, &policy, names);
        state.worker_wake.notify_one();
        return Ok(queued);
    }
    let directory = PathBuf::from(directory);
    if !directory.is_absolute() || !directory.is_dir() {
        return Err("Elige una carpeta de destino válida".into());
    }
    let queued = state
        .repository
        .enqueue_downloads(&ids, &directory, &policy);
    state.worker_wake.notify_one();
    Ok(queued)
}

#[tauri::command]
fn pause_transfer(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    let job = state
        .repository
        .transfer_by_id(&id)
        .map_err(|e| e.to_string())?
        .ok_or("Transferencia no encontrada")?;
    if matches!(
        job.status.as_str(),
        "uploading" | "downloading" | "analyzing" | "copying" | "confirming" | "running"
    ) {
        state
            .repository
            .request_pause(&id)
            .map_err(|e| e.to_string())
    } else if matches!(
        job.status.as_str(),
        "waiting" | "ready" | "queued" | "retry_wait" | "failed"
    ) {
        state.repository.mark_paused(&id).map_err(|e| e.to_string())
    } else {
        Err("Esta transferencia no se puede pausar".into())
    }
}

#[tauri::command]
fn cancel_transfer(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    let job = state
        .repository
        .transfer_by_id(&id)
        .map_err(|e| e.to_string())?
        .ok_or("Transferencia no encontrada")?;
    if matches!(
        job.status.as_str(),
        "uploading" | "downloading" | "analyzing" | "copying" | "confirming" | "running"
    ) {
        state
            .repository
            .request_cancel(&id)
            .map_err(|e| e.to_string())
    } else if !matches!(job.status.as_str(), "completed" | "duplicate" | "cancelled") {
        state
            .repository
            .mark_cancelled(&id)
            .map_err(|e| e.to_string())
    } else {
        Err("Esta transferencia ya terminó".into())
    }
}

async fn resume_one(state: Arc<AppState>, id: String) -> Result<(), String> {
    if state
        .repository
        .preparation_source(&id)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        let permit = state.preparation_slots.acquire().await;
        let owned = state.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let _permit = permit;
            TransferService::resume_preparation(&owned.repository, &id, &owned.staging_dir)
                .map(|_| ())
        })
        .await
        .map_err(|e| e.to_string())??;
        state.worker_wake.notify_one();
        return Ok(());
    }

    state.repository.resume(&id).map_err(|e| e.to_string())?;
    state.worker_wake.notify_one();
    Ok(())
}

#[tauri::command]
async fn resume_transfer(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    resume_one(state.inner().clone(), id).await
}

#[tauri::command]
async fn retry_transfer(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    resume_one(state.inner().clone(), id).await
}

#[tauri::command]
fn pause_queue(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let jobs = state
        .repository
        .list_transfers()
        .map_err(|e| e.to_string())?;
    for job in jobs {
        if matches!(
            job.status.as_str(),
            "waiting" | "ready" | "queued" | "retry_wait" | "failed"
        ) {
            let _ = state.repository.mark_paused(&job.id);
        } else if matches!(
            job.status.as_str(),
            "uploading" | "downloading" | "analyzing" | "copying" | "confirming" | "running"
        ) {
            let _ = state.repository.request_pause(&job.id);
        }
    }
    Ok(())
}

#[tauri::command]
async fn resume_queue(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let jobs = state
        .repository
        .list_transfers()
        .map_err(|e| e.to_string())?;
    let mut tasks = Vec::new();
    for job in jobs.into_iter().filter(|j| j.status == "paused") {
        let owned = state.inner().clone();
        tasks.push(tauri::async_runtime::spawn(async move {
            resume_one(owned, job.id).await
        }));
    }
    for task in tasks {
        task.await.map_err(|e| e.to_string())??;
    }
    Ok(())
}

#[tauri::command]
async fn set_trashed(
    state: State<'_, Arc<AppState>>,
    id: String,
    trashed: bool,
) -> Result<(), String> {
    let _guard = state.sync_lock.lock().await;
    state
        .telegram
        .set_files_trashed_synced(&state.repository, &[id], trashed)
        .await
        .map(|_| ())
}

#[tauri::command]
async fn set_trashed_many(
    state: State<'_, Arc<AppState>>,
    ids: Vec<String>,
    trashed: bool,
) -> Result<usize, String> {
    let _guard = state.sync_lock.lock().await;
    state
        .telegram
        .set_files_trashed_synced(&state.repository, &ids, trashed)
        .await
}

#[tauri::command]
async fn delete_files_permanently(
    state: State<'_, Arc<AppState>>,
    ids: Vec<String>,
) -> Result<usize, String> {
    let removed = state
        .telegram
        .delete_files_permanently(&state.repository, &ids)
        .await?;
    let _ = remove_media_cache_entries(&state.repository, &state.media_cache_dir, &ids);
    Ok(removed)
}

#[tauri::command]
async fn empty_trash(state: State<'_, Arc<AppState>>) -> Result<usize, String> {
    let ids = state.repository.trashed_ids().map_err(|e| e.to_string())?;
    if ids.is_empty() {
        return Ok(0);
    }
    let mut removed = 0usize;
    for chunk in ids.chunks(100) {
        let batch = chunk.to_vec();
        removed += state
            .telegram
            .delete_files_permanently(&state.repository, &batch)
            .await?;
        let _ = remove_media_cache_entries(&state.repository, &state.media_cache_dir, &batch);
    }
    Ok(removed)
}

#[tauri::command]
fn prepare_thumbnail_batch(
    state: State<'_, Arc<AppState>>,
    ids: Vec<String>,
) -> Result<Vec<media::ThumbnailBatchItem>, String> {
    if ids.len() > 64 {
        return Err("Solicita como máximo 64 miniaturas por lote".into());
    }
    let mut seen = std::collections::HashSet::with_capacity(ids.len());
    let mut output = Vec::with_capacity(ids.len());
    for id in ids {
        if !seen.insert(id.clone()) {
            continue;
        }
        output.push(media::ThumbnailBatchItem {
            source: media::cached_thumbnail_source(&state.repository, &id)?,
            id,
        });
    }
    Ok(output)
}

#[tauri::command]
async fn prepare_thumbnail(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<Option<media::ThumbnailSource>, String> {
    // Fast path for the progressive blurred preview captured during catalog sync.
    // This is local SQLite only: no semaphore, settings lookup or Telegram request.
    if let Some(source) = media::cached_thumbnail_source(&state.repository, &id)? {
        return Ok(Some(source));
    }

    static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
    let _slot = SLOTS.acquire().await.map_err(|e| e.to_string())?;
    let settings = state.repository.settings().map_err(|e| e.to_string())?;
    tokio::time::timeout(
        std::time::Duration::from_secs(45),
        state.telegram.prepare_thumbnail(
            &state.repository,
            &id,
            &state.media_cache_dir,
            settings.cache_limit_bytes,
        ),
    )
    .await
    .map_err(|_| "Miniatura no disponible por ahora".to_string())?
}

#[tauri::command]
async fn prepare_media(state: State<'_, Arc<AppState>>, id: String) -> Result<MediaReady, String> {
    let settings = state.repository.settings().map_err(|e| e.to_string())?;
    state
        .telegram
        .prepare_media(
            &state.repository,
            &id,
            &state.media_cache_dir,
            settings.cache_limit_bytes,
        )
        .await
}

#[tauri::command]
fn clear_media_cache_command(state: State<'_, Arc<AppState>>) -> Result<u64, String> {
    clear_media_cache(&state.repository, &state.media_cache_dir)
}

#[tauri::command]
fn clear_transfer_history(state: State<'_, Arc<AppState>>) -> Result<usize, String> {
    state
        .repository
        .clear_transfer_history()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn export_diagnostics(state: State<'_, Arc<AppState>>, destination: String) -> Result<(), String> {
    let settings = state.repository.settings().map_err(|e| e.to_string())?;
    let cache_bytes = state
        .repository
        .media_cache_total_bytes()
        .map_err(|e| e.to_string())?;
    let summary = state
        .repository
        .queue_summary(cache_bytes)
        .map_err(|e| e.to_string())?;
    let transfers = state
        .repository
        .list_transfers()
        .map_err(|e| e.to_string())?;
    let sanitized_transfers: Vec<_> = transfers
        .into_iter()
        .map(|job| {
            serde_json::json!({
                "id": job.id,
                "fileName": job.file_name,
                "direction": job.direction,
                "status": job.status,
                "phase": job.phase,
                "progress": job.progress,
                "processedBytes": job.processed_bytes,
                "totalBytes": job.total_bytes,
                "speedBps": job.speed_bps,
                "etaSeconds": job.eta_seconds,
                "attempts": job.attempts,
                "maxAttempts": job.max_attempts,
                "hasError": job.error.is_some(),
                "updatedAt": job.updated_at
            })
        })
        .collect();
    let report = serde_json::json!({
        "product": "Nuvio",
        "version": env!("CARGO_PKG_VERSION"),
        "generatedAtUnix": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
        "platform": std::env::consts::OS,
        "architecture": std::env::consts::ARCH,
        "telegramConnected": state.telegram.cached_snapshot().connected,
        "settings": settings,
        "queue": summary,
        "transfers": sanitized_transfers,
        "backgroundErrorPresent": state.background_error.lock().expect("background").is_some()
    });
    let bytes = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    #[cfg(target_os = "android")]
    if destination.starts_with("content://") {
        return mobile::write_document(&destination, &bytes);
    }
    let path = PathBuf::from(destination);
    if !path.is_absolute() {
        return Err("El destino del diagnóstico debe ser una ruta completa".into());
    }
    let parent = path.parent().ok_or("Destino de diagnóstico inválido")?;
    if !parent.is_dir() {
        return Err("La carpeta del diagnóstico no existe".into());
    }
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    use std::io::Write;
    temp.write_all(&bytes).map_err(|e| e.to_string())?;
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    temp.persist(&path).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn update_setting(
    state: State<'_, Arc<AppState>>,
    key: String,
    value: String,
) -> Result<AppSettings, String> {
    match key.as_str() {
        "remember_session" if value == "0" || value == "1" => {}
        "delete_original_after_upload" if value == "0" || value == "1" => {}
        "conflict_policy" if value == "skip" || value == "rename" => {}
        "cache_limit_bytes" => {
            let parsed: i64 = value.parse().map_err(|_| "Límite de caché inválido")?;
            if !(256 * 1024 * 1024..=20 * 1024 * 1024 * 1024_i64).contains(&parsed) {
                return Err("El límite de caché debe estar entre 256 MB y 20 GB".into());
            }
        }
        "preparation_concurrency" | "upload_concurrency" | "download_concurrency" => {
            let parsed: usize = value.parse().map_err(|_| "Concurrencia inválida")?;
            let max = if key == "upload_concurrency" { 16 } else { 8 };
            if !(1..=max).contains(&parsed) {
                return Err(format!("La concurrencia debe estar entre 1 y {max}"));
            }
        }
        "speed_limit_bps" => {
            if !value.is_empty() {
                let parsed: i64 = value.parse().map_err(|_| "Límite de velocidad inválido")?;
                if parsed <= 0 {
                    return Err("Límite de velocidad inválido".into());
                }
            }
        }
        _ => return Err("Ajuste no permitido".into()),
    }
    state
        .repository
        .set_setting(&key, &value)
        .map_err(|e| e.to_string())?;
    let settings = state.repository.settings().map_err(|e| e.to_string())?;
    match key.as_str() {
        "preparation_concurrency" => state
            .preparation_slots
            .set_limit(settings.preparation_concurrency),
        "upload_concurrency" => state.upload_slots.set_limit(settings.upload_concurrency),
        "download_concurrency" => state
            .download_slots
            .set_limit(settings.download_concurrency),
        _ => {}
    }
    if matches!(
        key.as_str(),
        "preparation_concurrency" | "upload_concurrency" | "download_concurrency"
    ) {
        state.worker_wake.notify_one();
    }
    Ok(settings)
}

#[tauri::command]
async fn update_mobile_sync_notification(data: serde_json::Value) -> Result<(), String> {
    mobile::update_sync_notification(&data)
}

#[tauri::command]
async fn update_mobile_upload_notification(data: serde_json::Value) -> Result<(), String> {
    mobile::update_upload_notification(&data)
}

#[tauri::command]
async fn clear_mobile_notification(id: i32) -> Result<(), String> {
    mobile::clear_notification(id)
}

async fn delete_verified_upload_source_inner(
    state: &Arc<AppState>,
    transfer_id: &str,
    require_opt_in: bool,
) -> Result<bool, String> {
    let Some(candidate) = state
        .repository
        .upload_source_cleanup(transfer_id, require_opt_in)
        .map_err(|e| e.to_string())?
    else {
        return Ok(false);
    };

    let result: Result<bool, String> = if candidate.source_path.starts_with("content://") {
        #[cfg(target_os = "android")]
        {
            mobile::delete_verified_document(
                &candidate.source_path,
                &candidate.sha256,
                candidate.size_bytes,
            )
            .await
        }
        #[cfg(not(target_os = "android"))]
        {
            Err("Un URI de Android no se puede eliminar desde Windows".into())
        }
    } else {
        let source = PathBuf::from(&candidate.source_path);
        let expected_hash = candidate.sha256.clone();
        let expected_size = candidate.size_bytes;
        tauri::async_runtime::spawn_blocking(move || {
            if !source.exists() {
                return Ok(false);
            }
            let metadata = fs::metadata(&source).map_err(|e| e.to_string())?;
            if !metadata.is_file() || metadata.len() != expected_size as u64 {
                return Err(
                    "El original cambió de tamaño después de la subida; se conservó por seguridad."
                        .into(),
                );
            }
            if sha256_file(&source).map_err(|e| e.to_string())? != expected_hash {
                return Err(
                    "El original cambió después de la subida; se conservó por seguridad.".into(),
                );
            }
            fs::remove_file(&source)
                .map_err(|e| format!("No se pudo borrar el original verificado: {e}"))?;
            Ok(true)
        })
        .await
        .map_err(|e| e.to_string())?
    };

    match result {
        Ok(removed) => {
            state
                .repository
                .mark_upload_source_deleted(transfer_id)
                .map_err(|e| e.to_string())?;
            Ok(removed)
        }
        Err(error) => {
            let _ = state
                .repository
                .mark_upload_source_delete_error(transfer_id, &error);
            Err(error)
        }
    }
}

#[tauri::command]
async fn delete_verified_upload_source(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<bool, String> {
    delete_verified_upload_source_inner(state.inner(), &id, false).await
}

fn uploaded_image_cleanup_candidates(
    state: &AppState,
) -> Result<Vec<repository::UploadSourceCleanupCandidate>, String> {
    let mut seen_sources = std::collections::HashSet::new();
    let candidates = state
        .repository
        .upload_source_cleanup_candidates()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|candidate| {
            let extension = Path::new(&candidate.file_name)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            cloud::classify_extension(&extension) == "image"
                && seen_sources.insert(candidate.source_path.clone())
        })
        .collect();
    Ok(candidates)
}

#[tauri::command]
fn uploaded_image_cleanup_summary(
    state: State<'_, Arc<AppState>>,
) -> Result<UploadedImageCleanupSummary, String> {
    let candidates = uploaded_image_cleanup_candidates(state.inner())?;
    Ok(UploadedImageCleanupSummary {
        count: candidates.len(),
        bytes: candidates
            .iter()
            .map(|candidate| candidate.size_bytes.max(0))
            .sum(),
    })
}

#[tauri::command]
async fn delete_uploaded_image_sources(
    state: State<'_, Arc<AppState>>,
) -> Result<UploadedImageCleanupResult, String> {
    let state = state.inner().clone();
    let candidates = uploaded_image_cleanup_candidates(&state)?;
    let mut result = UploadedImageCleanupResult {
        deleted: 0,
        released_bytes: 0,
        skipped: 0,
        failed: 0,
    };

    for candidate in candidates {
        let expected_release = if candidate.source_path.starts_with("content://") {
            candidate.size_bytes.max(0)
        } else {
            fs::metadata(&candidate.source_path)
                .ok()
                .filter(|metadata| metadata.is_file())
                .map(|metadata| metadata.len() as i64)
                .unwrap_or(0)
        };
        match delete_verified_upload_source_inner(&state, &candidate.transfer_id, false).await {
            Ok(true) => {
                result.deleted += 1;
                result.released_bytes = result
                    .released_bytes
                    .saturating_add(expected_release.min(candidate.size_bytes.max(0)));
            }
            Ok(false) => result.skipped += 1,
            Err(_) => result.failed += 1,
        }
    }
    Ok(result)
}

async fn run_job(state: Arc<AppState>, job: cloud::WorkItem) {
    let result = if job.direction == "upload" {
        state.telegram.run_upload(&state.repository, &job).await
    } else {
        state.telegram.run_download(&state.repository, &job).await
    };
    if result.is_ok() && job.direction == "upload" {
        let _ = delete_verified_upload_source_inner(&state, &job.id, true).await;
    }
    if let Err(error) = result {
        let current = state.repository.transfer_by_id(&job.id).ok().flatten();
        let already_terminal = current.as_ref().is_some_and(|j| {
            matches!(
                j.status.as_str(),
                "paused" | "cancelled" | "completed" | "duplicate"
            )
        });
        if !already_terminal {
            if let Err(db) = state.repository.mark_retry_or_failed(&job.id, &error) {
                *state.background_error.lock().expect("background") = Some(db.to_string());
            }
        }
    }
    state.worker_wake.notify_one();
}

async fn worker(state: Arc<AppState>) {
    let settings = state.repository.settings().unwrap_or_default();
    if let Err(error) = state.telegram.initialize(settings.remember_session).await {
        *state.background_error.lock().expect("background") = Some(error);
    }
    let mut bootstrap_checked = false;
    let mut first_iteration = true;
    loop {
        if first_iteration {
            first_iteration = false;
        } else {
            let wait = state
                .repository
                .next_retry_delay()
                .ok()
                .flatten()
                .unwrap_or_else(|| Duration::from_secs(30))
                .min(Duration::from_secs(30));
            tokio::select! {
                _ = state.worker_wake.notified() => {},
                _ = state.telegram.realtime_notify.notified() => {},
                _ = tokio::time::sleep(wait) => {},
            }
        }
        let _ = state.repository.release_due_retries();
        if !state.telegram.cached_snapshot().connected {
            // A later login may point to a different Telegram account/chat, whose
            // bootstrap marker is scoped independently.
            bootstrap_checked = false;
            continue;
        }

        if let Err(error) = state
            .telegram
            .apply_realtime_updates(&state.repository)
            .await
        {
            *state.background_error.lock().expect("background") = Some(error);
        }
        if state.telegram.realtime_overflowed() {
            if let Ok(_guard) = state.sync_lock.try_lock() {
                match state
                    .telegram
                    .sync_catalog_checked(&state.repository, false)
                    .await
                {
                    Ok(_) => state.telegram.clear_realtime_overflow(),
                    Err(error) => {
                        *state.background_error.lock().expect("background") = Some(error);
                    }
                }
            }
        }

        // Startup is local-first. Only a genuinely new account/catalog performs one
        // automatic bootstrap. Existing installations load SQLite immediately and do
        // not hit Telegram again until the user presses Sincronizar (or an upload
        // recovery path explicitly needs remote confirmation).
        if !bootstrap_checked {
            match state.repository.catalog_bootstrap_required_locally() {
                Ok(false) => {
                    // Existing catalog: startup remains fully local. Do not touch the
                    // Telegram catalog until the user explicitly presses Sincronizar.
                    bootstrap_checked = true;
                }
                Ok(true) => {
                    if let Ok(_guard) = state.sync_lock.try_lock() {
                        match state
                            .telegram
                            .bootstrap_catalog_if_needed(&state.repository)
                            .await
                        {
                            Ok(_) => {
                                bootstrap_checked = true;
                                *state.background_error.lock().expect("background") = None;
                            }
                            Err(error) => {
                                *state.background_error.lock().expect("background") = Some(error);
                                tokio::time::sleep(Duration::from_secs(5)).await;
                                continue;
                            }
                        }
                    }
                }
                Err(error) => {
                    *state.background_error.lock().expect("background") = Some(error);
                    bootstrap_checked = true;
                }
            }
        }

        loop {
            let Some(permit) = state.upload_slots.try_acquire() else {
                break;
            };
            match state.repository.claim_pending("upload") {
                Ok(Some(job)) => {
                    let owned = state.clone();
                    tauri::async_runtime::spawn(async move {
                        let _permit = permit;
                        run_job(owned, job).await;
                    });
                }
                Ok(None) => {
                    drop(permit);
                    break;
                }
                Err(error) => {
                    drop(permit);
                    *state.background_error.lock().expect("background") = Some(error.to_string());
                    break;
                }
            }
        }

        loop {
            let Some(permit) = state.download_slots.try_acquire() else {
                break;
            };
            match state.repository.claim_pending("download") {
                Ok(Some(job)) => {
                    let owned = state.clone();
                    tauri::async_runtime::spawn(async move {
                        let _permit = permit;
                        run_job(owned, job).await;
                    });
                }
                Ok(None) => {
                    drop(permit);
                    break;
                }
                Err(error) => {
                    drop(permit);
                    *state.background_error.lock().expect("background") = Some(error.to_string());
                    break;
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DroppedPathInfo {
    pub path: String,
    pub is_dir: bool,
    pub name: String,
}

#[tauri::command]
fn inspect_dropped_paths(paths: Vec<String>) -> Vec<DroppedPathInfo> {
    paths
        .into_iter()
        .map(|p| {
            let path_buf = PathBuf::from(&p);
            let is_dir = path_buf.is_dir();
            let name = path_buf
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| p.clone());
            DroppedPathInfo {
                path: p,
                is_dir,
                name,
            }
        })
        .collect()
}

#[tauri::command]
fn decrypt_nuvio_file(
    input_path: String,
    output_path: String,
    mut passphrase: String,
) -> Result<(), String> {
    let result = decrypt_file(Path::new(&input_path), Path::new(&output_path), &passphrase)
        .map_err(|error| error.to_string());
    passphrase.zeroize();
    result
}

fn wake_worker_after_auth(
    state: &AppState,
    result: Result<TelegramAuthSnapshot, String>,
) -> Result<TelegramAuthSnapshot, String> {
    if result.as_ref().is_ok_and(|snapshot| snapshot.connected) {
        state.worker_wake.notify_one();
    }
    result
}

#[tauri::command]
async fn telegram_auth_state(
    state: State<'_, Arc<AppState>>,
) -> Result<TelegramAuthSnapshot, String> {
    wake_worker_after_auth(state.inner(), state.telegram.refresh().await)
}

#[tauri::command]
async fn telegram_configure(
    state: State<'_, Arc<AppState>>,
    api_id: i32,
    api_hash: String,
    remember_session: bool,
) -> Result<TelegramAuthSnapshot, String> {
    state
        .repository
        .set_setting("remember_session", if remember_session { "1" } else { "0" })
        .map_err(|e| e.to_string())?;
    wake_worker_after_auth(
        state.inner(),
        state
            .telegram
            .configure(api_id, api_hash, remember_session)
            .await,
    )
}

#[tauri::command]
async fn telegram_submit_phone(
    state: State<'_, Arc<AppState>>,
    phone: String,
    delivery: Option<String>,
) -> Result<TelegramAuthSnapshot, String> {
    wake_worker_after_auth(
        state.inner(),
        state
            .telegram
            .submit_phone(phone, delivery.as_deref().unwrap_or("telegram"))
            .await,
    )
}
#[tauri::command]
async fn telegram_reset_to_phone(
    state: State<'_, Arc<AppState>>,
) -> Result<TelegramAuthSnapshot, String> {
    state.telegram.reset_to_phone().await
}
#[tauri::command]
async fn telegram_submit_email(
    state: State<'_, Arc<AppState>>,
    email: String,
) -> Result<TelegramAuthSnapshot, String> {
    state.telegram.submit_email(email).await
}
#[tauri::command]
async fn telegram_submit_email_code(
    state: State<'_, Arc<AppState>>,
    code: String,
) -> Result<TelegramAuthSnapshot, String> {
    wake_worker_after_auth(state.inner(), state.telegram.submit_email_code(code).await)
}
#[tauri::command]
async fn telegram_submit_email_identity(
    state: State<'_, Arc<AppState>>,
    provider: String,
    token: String,
) -> Result<TelegramAuthSnapshot, String> {
    wake_worker_after_auth(
        state.inner(),
        state.telegram.submit_email_identity(&provider, token).await,
    )
}

#[tauri::command]
async fn telegram_reset_authentication_email(
    state: State<'_, Arc<AppState>>,
) -> Result<TelegramAuthSnapshot, String> {
    state.telegram.reset_authentication_email().await
}

#[tauri::command]
async fn telegram_login_email_status(
    state: State<'_, Arc<AppState>>,
) -> Result<TelegramLoginEmailStatus, String> {
    state.telegram.login_email_status().await
}

#[tauri::command]
async fn telegram_set_login_email(
    state: State<'_, Arc<AppState>>,
    email: String,
) -> Result<TelegramLoginEmailCodeInfo, String> {
    state.telegram.set_login_email(email).await
}

#[tauri::command]
async fn telegram_resend_login_email(
    state: State<'_, Arc<AppState>>,
) -> Result<TelegramLoginEmailCodeInfo, String> {
    state.telegram.resend_login_email().await
}

#[tauri::command]
async fn telegram_check_login_email(
    state: State<'_, Arc<AppState>>,
    code: String,
) -> Result<TelegramAuthSnapshot, String> {
    wake_worker_after_auth(state.inner(), state.telegram.check_login_email(code).await)
}

#[tauri::command]
async fn telegram_submit_code(
    state: State<'_, Arc<AppState>>,
    code: String,
) -> Result<TelegramAuthSnapshot, String> {
    wake_worker_after_auth(state.inner(), state.telegram.submit_code(code).await)
}
#[tauri::command]
async fn telegram_resend_code(
    state: State<'_, Arc<AppState>>,
    delivery: Option<String>,
) -> Result<TelegramAuthSnapshot, String> {
    state.telegram.resend_code(delivery.as_deref()).await
}
#[tauri::command]
async fn telegram_submit_password(
    state: State<'_, Arc<AppState>>,
    password: String,
) -> Result<TelegramAuthSnapshot, String> {
    wake_worker_after_auth(
        state.inner(),
        state.telegram.submit_password(password).await,
    )
}
#[tauri::command]
async fn telegram_request_qr(
    state: State<'_, Arc<AppState>>,
) -> Result<TelegramAuthSnapshot, String> {
    wake_worker_after_auth(state.inner(), state.telegram.request_qr().await)
}
#[tauri::command]
async fn telegram_register_user(
    state: State<'_, Arc<AppState>>,
    first_name: String,
    last_name: String,
) -> Result<TelegramAuthSnapshot, String> {
    wake_worker_after_auth(
        state.inner(),
        state.telegram.register_user(first_name, last_name).await,
    )
}
#[tauri::command]
async fn telegram_log_out(state: State<'_, Arc<AppState>>) -> Result<TelegramAuthSnapshot, String> {
    if state.repository.active_count().map_err(|e| e.to_string())? > 0 {
        return Err("Pausa o espera las transferencias activas antes de cerrar sesión".into());
    }
    state.telegram.log_out().await
}
#[tauri::command]
async fn telegram_forget_session(
    state: State<'_, Arc<AppState>>,
) -> Result<TelegramAuthSnapshot, String> {
    if state.repository.active_count().map_err(|e| e.to_string())? > 0 {
        return Err("Pausa o espera las transferencias activas antes de olvidar la sesión".into());
    }
    state
        .repository
        .set_setting("remember_session", "0")
        .map_err(|e| e.to_string())?;
    state.telegram.forget_session().await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(mobile::init())
        .setup(|app| {
            let app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|error| format!("unable to resolve app data directory: {error}"))?;
            fs::create_dir_all(&app_data_dir)
                .map_err(|error| format!("unable to create app data directory: {error}"))?;
            let staging_dir = app_data_dir.join("staging");
            let media_cache_dir = app
                .path()
                .app_cache_dir()
                .map_err(|error| format!("unable to resolve app cache directory: {error}"))?
                .join("media");
            fs::create_dir_all(&staging_dir).map_err(|error| error.to_string())?;
            fs::create_dir_all(&media_cache_dir).map_err(|error| error.to_string())?;

            let repository = CatalogRepository::open(&app_data_dir.join("nuvio.db"))
                .map_err(|error| error.to_string())?;
            repository.init_cloud()?;
            reconcile_media_cache_index(&repository, &media_cache_dir)?;
            let settings = repository.settings().map_err(|e| e.to_string())?;
            let telegram = TelegramService::new(&app_data_dir)?;
            let state = Arc::new(AppState {
                repository,
                telegram,
                staging_dir,
                media_cache_dir,
                preparation_slots: DynamicLimiter::new(settings.preparation_concurrency, 8),
                upload_slots: DynamicLimiter::new(settings.upload_concurrency, 16),
                download_slots: DynamicLimiter::new(settings.download_concurrency, 8),
                worker_wake: Arc::new(tokio::sync::Notify::new()),
                sync_lock: tokio::sync::Mutex::new(()),
                background_error: Mutex::new(None),
            });
            app.manage(state.clone());
            tauri::async_runtime::spawn(worker(state));
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if let Some(state) = window.try_state::<Arc<AppState>>() {
                    if state.repository.active_count().unwrap_or(0) > 0 {
                        let _ = state.repository.pause_all_active();
                        api.prevent_close();
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_dashboard,
            get_dashboard_status,
            get_catalog_page,
            get_sync_delta,
            set_favorite,
            analyze_upload_selection,
            prepare_zip_uploads,
            prepare_upload,
            scan_directory_for_upload,
            inspect_dropped_paths,
            pick_upload_directory,
            pick_upload_files,
            decrypt_nuvio_file,
            sync_files,
            create_folder,
            rename_folder,
            move_folder,
            delete_folder,
            move_files_to_folder,
            queue_downloads,
            pick_download_directory,
            platform_name,
            background_app,
            pause_transfer,
            cancel_transfer,
            resume_transfer,
            retry_transfer,
            pause_queue,
            resume_queue,
            set_trashed,
            set_trashed_many,
            delete_files_permanently,
            empty_trash,
            prepare_media,
            prepare_thumbnail_batch,
            prepare_thumbnail,
            clear_media_cache_command,
            clear_transfer_history,
            export_diagnostics,
            delete_verified_upload_source,
            uploaded_image_cleanup_summary,
            delete_uploaded_image_sources,
            update_setting,
            telegram_auth_state,
            telegram_configure,
            telegram_submit_phone,
            telegram_reset_to_phone,
            telegram_submit_email,
            telegram_submit_email_code,
            telegram_submit_email_identity,
            telegram_reset_authentication_email,
            telegram_login_email_status,
            telegram_set_login_email,
            telegram_resend_login_email,
            telegram_check_login_email,
            telegram_submit_code,
            telegram_resend_code,
            telegram_submit_password,
            telegram_request_qr,
            telegram_register_user,
            telegram_log_out,
            telegram_forget_session,
            update_mobile_sync_notification,
            update_mobile_upload_notification,
            clear_mobile_notification
        ])
        .run(tauri::generate_context!())
        .expect("error while running Nuvio");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_directory_upload_plan_hierarchy_and_filtering() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("MiProyecto");
        fs::create_dir_all(&root).unwrap();

        // Create nested directories
        let sub_docs = root.join("docs");
        let sub_images = root.join("images");
        let sub_nested = sub_images.join("2026");
        let sub_empty = root.join("vacia");

        fs::create_dir_all(&sub_docs).unwrap();
        fs::create_dir_all(&sub_images).unwrap();
        fs::create_dir_all(&sub_nested).unwrap();
        fs::create_dir_all(&sub_empty).unwrap();

        // Create files
        fs::write(root.join("readme.txt"), b"hola").unwrap();
        fs::write(sub_docs.join("manual.pdf"), b"documento").unwrap();
        fs::write(sub_nested.join("foto.jpg"), b"imagen").unwrap();

        // Create ignored files
        fs::write(root.join(".gitignore"), b"node_modules").unwrap();
        fs::write(sub_images.join("Thumbs.db"), b"cache").unwrap();
        fs::write(sub_docs.join(".DS_Store"), b"apple").unwrap();

        let plan = build_directory_upload_plan(&root).unwrap();

        assert_eq!(plan.root_name, "MiProyecto");
        assert_eq!(plan.files.len(), 3);

        // Check relative paths of files
        let file_rel_paths: Vec<&str> = plan
            .files
            .iter()
            .map(|f| f.relative_path.as_str())
            .collect();
        assert!(file_rel_paths.contains(&"readme.txt"));
        assert!(file_rel_paths.contains(&"docs/manual.pdf"));
        assert!(file_rel_paths.contains(&"images/2026/foto.jpg"));
        assert!(!file_rel_paths.iter().any(|p| p.contains(".gitignore")
            || p.contains("Thumbs.db")
            || p.contains(".DS_Store")));

        // Folders must include all subdirectories in shallowest-first order
        assert!(plan.folders.contains(&"docs".to_string()));
        assert!(plan.folders.contains(&"images".to_string()));
        assert!(plan.folders.contains(&"vacia".to_string()));
        assert!(plan.folders.contains(&"images/2026".to_string()));

        let idx_images = plan.folders.iter().position(|f| f == "images").unwrap();
        let idx_nested = plan
            .folders
            .iter()
            .position(|f| f == "images/2026")
            .unwrap();
        assert!(
            idx_images < idx_nested,
            "Parent folder must appear before nested subfolder"
        );

        assert_eq!(plan.total_bytes, 4 + 9 + 6);
    }

    #[test]
    fn directory_upload_limits_are_bounded_before_large_ipc_payloads() {
        assert!(validate_directory_upload_limits(10_000, 10_000, 128).is_ok());
        assert!(validate_directory_upload_limits(10_001, 0, 0).is_err());
        assert!(validate_directory_upload_limits(0, 10_001, 0).is_err());
        assert!(validate_directory_upload_limits(0, 0, 129).is_err());
    }

    #[test]
    fn test_build_directory_upload_plan_errors() {
        let temp = tempfile::tempdir().unwrap();
        let non_existent = temp.path().join("does_not_exist");
        assert!(build_directory_upload_plan(&non_existent).is_err());

        let file_path = temp.path().join("file.txt");
        fs::write(&file_path, b"not a dir").unwrap();
        assert!(build_directory_upload_plan(&file_path).is_err());
    }
}
