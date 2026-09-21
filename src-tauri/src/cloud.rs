use crate::{
    crypto::sha256_file,
    progress::SpeedEstimator,
    repository::{CatalogRepository, RepositoryError},
    telegram::{call, TelegramRealtimeUpdate, TelegramService},
};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const SYNC_CANCELLED_ERROR: &str = "Sincronización detenida por el usuario";
use tdlib_rs::{enums as e, functions as f, types as t};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteDocument {
    pub transfer: String,
    pub message_id: i64,
    pub name: String,
    pub size: i64,
    pub date: i32,
    pub sha256: String,
    pub folder_id: Option<String>,
    #[serde(default)]
    pub minithumbnail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorkItem {
    pub id: String,
    pub path: String,
    pub size: i64,
    pub sha256: String,
    pub direction: String,
    pub pending: Option<i64>,
    pub folder_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDownloadItem {
    pub file_id: String,
    pub transfer_id: Option<String>,
    pub status: String,
    pub path: Option<String>,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Caption {
    v: u8,
    transfer: String,
    sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FolderEvent {
    v: u8,
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
    trashed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileMoveEvent {
    v: u8,
    message_ids: Vec<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileTrashEvent {
    v: u8,
    message_ids: Vec<i64>,
    trashed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileDeleteEvent {
    v: u8,
    message_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct CatalogSnapshotCheckpoints {
    documents: i64,
    folders: i64,
    moves: i64,
    trash: i64,
    deletes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CatalogSnapshotV2 {
    v: u8,
    generated_at: i64,
    checkpoints: CatalogSnapshotCheckpoints,
    documents: Vec<RemoteDocument>,
    folders: Vec<FolderEvent>,
    trashed_message_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CatalogSnapshotCaption {
    v: u8,
    sha256: String,
    documents: usize,
    generated_at: i64,
}

fn snapshot_unix_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

fn valid_snapshot_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_catalog_snapshot(snapshot: &CatalogSnapshotV2) -> Result<(), String> {
    if snapshot.v != 2 {
        return Err("Versión de snapshot de catálogo no compatible".into());
    }
    if snapshot.documents.len() > 1_000_000
        || snapshot.folders.len() > 200_000
        || snapshot.trashed_message_ids.len() > 1_000_000
    {
        return Err("El snapshot de catálogo excede los límites de seguridad".into());
    }
    if [
        snapshot.checkpoints.documents,
        snapshot.checkpoints.folders,
        snapshot.checkpoints.moves,
        snapshot.checkpoints.trash,
        snapshot.checkpoints.deletes,
    ]
    .into_iter()
    .any(|value| value < 0)
    {
        return Err("El snapshot contiene checkpoints inválidos".into());
    }

    let mut document_ids = std::collections::HashSet::with_capacity(snapshot.documents.len());
    for document in &snapshot.documents {
        if document.message_id <= 0
            || document.size < 0
            || !valid_snapshot_hash(&document.sha256)
            || !document_ids.insert(document.message_id)
        {
            return Err("El snapshot contiene un documento inválido o duplicado".into());
        }
    }

    let mut folder_ids = std::collections::HashSet::with_capacity(snapshot.folders.len());
    for folder in &snapshot.folders {
        if folder.v != 1
            || folder.id.is_empty()
            || !folder_ids.insert(folder.id.as_str())
            || crate::repository::validate_folder_name(&folder.name).is_err()
        {
            return Err("El snapshot contiene una carpeta inválida o duplicada".into());
        }
    }
    for folder in &snapshot.folders {
        if folder
            .parent_id
            .as_deref()
            .is_some_and(|parent| parent == folder.id || !folder_ids.contains(parent))
        {
            return Err("El snapshot contiene una jerarquía de carpetas inválida".into());
        }
    }
    if snapshot
        .trashed_message_ids
        .iter()
        .any(|message_id| *message_id <= 0 || !document_ids.contains(message_id))
    {
        return Err("El snapshot contiene referencias de Papelera inválidas".into());
    }
    Ok(())
}

fn write_catalog_snapshot_zip(snapshot: &CatalogSnapshotV2, path: &Path) -> Result<(), String> {
    validate_catalog_snapshot(snapshot)?;
    let file = fs::File::create(path).map_err(|error| error.to_string())?;
    let mut zip = ZipWriter::new(file);
    zip.start_file(
        "catalog.json",
        SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(1)),
    )
    .map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut zip, snapshot).map_err(|error| error.to_string())?;
    let file = zip.finish().map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    Ok(())
}

fn read_catalog_snapshot_zip(path: &Path) -> Result<CatalogSnapshotV2, String> {
    const MAX_SNAPSHOT_JSON_BYTES: u64 = 512 * 1024 * 1024;
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|error| error.to_string())?;
    let entry = archive
        .by_name("catalog.json")
        .map_err(|_| "El snapshot no contiene catalog.json".to_string())?;
    if entry.size() > MAX_SNAPSHOT_JSON_BYTES {
        return Err("El snapshot de catálogo descomprimido es demasiado grande".into());
    }
    let snapshot: CatalogSnapshotV2 =
        serde_json::from_reader(entry.take(MAX_SNAPSHOT_JSON_BYTES + 1))
            .map_err(|error| error.to_string())?;
    validate_catalog_snapshot(&snapshot)?;
    Ok(snapshot)
}

fn parse_catalog_snapshot_caption(text: &str) -> Option<CatalogSnapshotCaption> {
    let payload = text.strip_prefix("#NuvioCatalog2 ")?;
    let caption: CatalogSnapshotCaption = serde_json::from_str(payload).ok()?;
    if caption.v != 2
        || caption.documents > 1_000_000
        || caption.generated_at <= 0
        || !valid_snapshot_hash(&caption.sha256)
    {
        return None;
    }
    Some(caption)
}

fn catalog_snapshot_message(message: &t::Message) -> Option<(CatalogSnapshotCaption, t::File)> {
    if message.sending_state.is_some() {
        return None;
    }
    let e::MessageContent::MessageDocument(content) = &message.content else {
        return None;
    };
    let caption = parse_catalog_snapshot_caption(&content.caption.text)?;
    let file = content.document.document.clone();
    // A catalog snapshot is metadata only. Bound the compressed payload before
    // asking TDLib to download it; the decompressed JSON has its own stricter cap.
    let advertised_size = file.size.max(file.expected_size);
    if !(0..=512 * 1024 * 1024).contains(&advertised_size) {
        return None;
    }
    Some((caption, file))
}

struct HistoryScanState {
    folders: std::collections::HashMap<String, FolderEvent>,
    moves: std::collections::HashMap<i64, Option<String>>,
    trash_states: std::collections::HashMap<i64, bool>,
    deleted_message_ids: std::collections::HashSet<i64>,
    scanned: usize,
    total_documents: usize,
    newest: i64,
}

#[derive(Debug, Default)]
struct FastSearchOutcome {
    fetched: usize,
    total_hint: i32,
    oldest_message_id: Option<i64>,
    exhausted: bool,
    stalled: bool,
}

#[cfg(any(test, target_os = "android"))]
#[derive(Serialize, Deserialize)]
struct AndroidDestination {
    tree: String,
    name: String,
    policy: String,
}

impl CatalogRepository {
    fn sync_checkpoint(&self, chat: i64) -> Result<Option<i64>, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let value: Option<String> = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [format!("catalog_sync_v1:{chat}")],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        value
            .map(|v| v.parse::<i64>().map_err(|e| e.to_string()))
            .transpose()
    }

    fn save_sync_checkpoint(&self, chat: i64, message: i64) -> Result<(), String> {
        self.connection.lock().map_err(|e| e.to_string())?.execute(
            "INSERT INTO app_meta(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![format!("catalog_sync_v1:{chat}"), message.to_string()],
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn sync_stream_checkpoint(
        &self,
        chat: i64,
        stream: &str,
        fallback: Option<i64>,
    ) -> Result<Option<i64>, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let key = format!("catalog_sync_v2:{chat}:{stream}");
        let value: Option<String> = connection
            .query_row("SELECT value FROM app_meta WHERE key=?1", [&key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| e.to_string())?;
        value
            .map(|v| v.parse::<i64>().map_err(|e| e.to_string()))
            .transpose()
            .map(|stored| stored.or(fallback))
    }

    fn save_sync_stream_checkpoint(
        &self,
        chat: i64,
        stream: &str,
        message: i64,
    ) -> Result<(), String> {
        self.connection
            .lock()
            .map_err(|e| e.to_string())?
            .execute(
                "INSERT INTO app_meta(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![format!("catalog_sync_v2:{chat}:{stream}"), message.to_string()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn fast_search_backfill_done(&self, chat: i64) -> Result<bool, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let value: Option<String> = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [format!("catalog_fast_search_v4:{chat}")],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(value.as_deref() == Some("1"))
    }

    fn mark_fast_search_backfill_done(&self, chat: i64) -> Result<(), String> {
        self.connection
            .lock()
            .map_err(|e| e.to_string())?
            .execute(
                "INSERT INTO app_meta(key,value) VALUES (?1,'1') ON CONFLICT(key) DO UPDATE SET value='1'",
                [format!("catalog_fast_search_v4:{chat}")],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn catalog_remote_count(&self) -> Result<usize, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM files WHERE provider='telegram' AND telegram_message_id IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(count.max(0) as usize)
    }

    fn history_backfill_done(&self, chat: i64) -> Result<bool, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let value: Option<String> = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [format!("catalog_history_backfill_v3:{chat}")],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(value.as_deref() == Some("1"))
    }

    fn mark_history_backfill_done(&self, chat: i64) -> Result<(), String> {
        self.connection
            .lock()
            .map_err(|e| e.to_string())?
            .execute(
                "INSERT INTO app_meta(key,value) VALUES (?1,'1') ON CONFLICT(key) DO UPDATE SET value='1'",
                [format!("catalog_history_backfill_v3:{chat}")],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn catalog_oldest_message_id(&self) -> Result<Option<i64>, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let raw: Option<String> = connection
            .query_row(
                "SELECT telegram_message_id FROM files
                 WHERE provider='telegram' AND telegram_message_id IS NOT NULL
                 ORDER BY CAST(telegram_message_id AS INTEGER) ASC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        raw.map(|value| {
            value
                .parse::<i64>()
                .map_err(|_| "El catálogo contiene un identificador remoto inválido".to_string())
        })
        .transpose()
    }

    pub fn catalog_bootstrap_required_locally(&self) -> Result<bool, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let existing: i64 = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM app_meta
                    WHERE (key LIKE 'catalog_initial_sync_v1:%' AND value='1')
                       OR key LIKE 'catalog_sync_v1:%'
                )",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(existing == 0)
    }

    fn initial_catalog_sync_needed(&self, chat: i64) -> Result<bool, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let key = format!("catalog_initial_sync_v1:{chat}");
        let done: Option<String> = connection
            .query_row("SELECT value FROM app_meta WHERE key=?1", [&key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| e.to_string())?;
        if done.as_deref() == Some("1") {
            return Ok(false);
        }
        drop(connection);

        // Existing installations already have a chat-scoped catalog checkpoint.
        // Treat that as a completed bootstrap so upgrading never triggers a full
        // automatic rescan just because this marker is new.
        if self.sync_checkpoint(chat)?.is_some() {
            self.mark_initial_catalog_sync_done(chat)?;
            return Ok(false);
        }
        Ok(true)
    }

    fn mark_initial_catalog_sync_done(&self, chat: i64) -> Result<(), String> {
        self.connection
            .lock()
            .map_err(|e| e.to_string())?
            .execute(
                "INSERT INTO app_meta(key,value) VALUES (?1,'1') ON CONFLICT(key) DO UPDATE SET value='1'",
                [format!("catalog_initial_sync_v1:{chat}")],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn export_catalog_snapshot(&self, chat: i64) -> Result<CatalogSnapshotV2, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;

        let general_checkpoint: Option<i64> = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [format!("catalog_sync_v1:{chat}")],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .map(|value| value.parse::<i64>().map_err(|e| e.to_string()))
            .transpose()?;
        let general_checkpoint = general_checkpoint.unwrap_or(0).max(0);
        let stream_checkpoint = |stream: &str| -> Result<i64, String> {
            let key = format!("catalog_sync_v2:{chat}:{stream}");
            let value: Option<String> = connection
                .query_row("SELECT value FROM app_meta WHERE key=?1", [&key], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(|e| e.to_string())?;
            value
                .map(|value| value.parse::<i64>().map_err(|e| e.to_string()))
                .transpose()
                .map(|value| value.unwrap_or(general_checkpoint).max(0))
        };

        let checkpoints = CatalogSnapshotCheckpoints {
            documents: stream_checkpoint("documents")?,
            folders: stream_checkpoint("folders")?,
            moves: stream_checkpoint("moves")?,
            trash: stream_checkpoint("trash")?,
            deletes: stream_checkpoint("deletes")?,
        };

        let mut documents = Vec::new();
        {
            let mut statement = connection
                .prepare("SELECT document FROM remote_documents")
                .map_err(|e| e.to_string())?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            for row in rows {
                let raw = row.map_err(|e| e.to_string())?;
                let mut document: RemoteDocument =
                    serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                // Progressive mini-thumbnails are intentionally excluded from the
                // portable catalog. They can dwarf metadata for 100k+ libraries and
                // are cheaply re-fetched on demand.
                document.minithumbnail = None;
                documents.push(document);
            }
        }
        documents.sort_by_key(|document| document.message_id);

        let mut folders = Vec::new();
        {
            let mut statement = connection
                .prepare(
                    "SELECT id,name,parent_id,trashed
                     FROM folders
                     ORDER BY id",
                )
                .map_err(|e| e.to_string())?;
            let rows = statement
                .query_map([], |row| {
                    Ok(FolderEvent {
                        v: 1,
                        id: row.get(0)?,
                        name: row.get(1)?,
                        parent_id: row.get(2)?,
                        trashed: row.get::<_, i64>(3)? != 0,
                    })
                })
                .map_err(|e| e.to_string())?;
            for row in rows {
                folders.push(row.map_err(|e| e.to_string())?);
            }
        }

        let mut trashed_message_ids = Vec::new();
        {
            let mut statement = connection
                .prepare(
                    "SELECT telegram_message_id
                     FROM files
                     WHERE provider='telegram'
                       AND trashed=1
                       AND telegram_message_id IS NOT NULL
                     ORDER BY CAST(telegram_message_id AS INTEGER)",
                )
                .map_err(|e| e.to_string())?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            for row in rows {
                let raw = row.map_err(|e| e.to_string())?;
                trashed_message_ids.push(
                    raw.parse::<i64>()
                        .map_err(|_| "Identificador remoto inválido en el catálogo".to_string())?,
                );
            }
        }

        let snapshot = CatalogSnapshotV2 {
            v: 2,
            generated_at: snapshot_unix_time(),
            checkpoints,
            documents,
            folders,
            trashed_message_ids,
        };
        validate_catalog_snapshot(&snapshot)?;
        Ok(snapshot)
    }

    fn import_catalog_snapshot(
        &self,
        chat: i64,
        snapshot: &CatalogSnapshotV2,
    ) -> Result<usize, String> {
        validate_catalog_snapshot(snapshot)?;
        if self.catalog_remote_count()? != 0 {
            return Err("El snapshot solo puede importarse sobre un catálogo remoto vacío".into());
        }

        self.apply_folder_snapshot(snapshot.folders.clone())?;
        for batch in snapshot.documents.chunks(1000) {
            self.record_remote_batch(batch).map_err(|e| e.to_string())?;
        }
        let trash_states = snapshot
            .trashed_message_ids
            .iter()
            .copied()
            .map(|message_id| (message_id, true))
            .collect::<std::collections::HashMap<_, _>>();
        self.reconcile_trash_states(&trash_states)?;

        let checkpoints = &snapshot.checkpoints;
        self.save_sync_checkpoint(
            chat,
            [
                checkpoints.documents,
                checkpoints.folders,
                checkpoints.moves,
                checkpoints.trash,
                checkpoints.deletes,
            ]
            .into_iter()
            .max()
            .unwrap_or(0),
        )?;
        self.save_sync_stream_checkpoint(chat, "documents", checkpoints.documents)?;
        self.save_sync_stream_checkpoint(chat, "folders", checkpoints.folders)?;
        self.save_sync_stream_checkpoint(chat, "moves", checkpoints.moves)?;
        self.save_sync_stream_checkpoint(chat, "trash", checkpoints.trash)?;
        self.save_sync_stream_checkpoint(chat, "deletes", checkpoints.deletes)?;
        self.mark_history_backfill_done(chat)?;
        self.mark_fast_search_backfill_done(chat)?;
        self.mark_trash_backfill_done(chat)?;
        self.mark_initial_catalog_sync_done(chat)?;
        Ok(snapshot.documents.len())
    }

    fn snapshot_publish_needed(&self, chat: i64, cursor: i64) -> Result<bool, String> {
        if cursor <= 0 {
            return Ok(false);
        }
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let now: i64 = connection
            .query_row("SELECT unixepoch()", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        let last_cursor: Option<String> = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [format!("catalog_snapshot_v2_cursor:{chat}")],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let last_at: Option<String> = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [format!("catalog_snapshot_v2_at:{chat}")],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let last_cursor = last_cursor
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let last_at = last_at
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        Ok(cursor > last_cursor
            && (last_cursor == 0
                || cursor.saturating_sub(last_cursor) >= 1000
                || now.saturating_sub(last_at) >= 6 * 60 * 60))
    }

    fn mark_snapshot_published(
        &self,
        chat: i64,
        cursor: i64,
        message_id: i64,
    ) -> Result<(), String> {
        let mut connection = self.connection.lock().map_err(|e| e.to_string())?;
        let transaction = connection.transaction().map_err(|e| e.to_string())?;
        let now: i64 = transaction
            .query_row("SELECT unixepoch()", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        for (key, value) in [
            (
                format!("catalog_snapshot_v2_cursor:{chat}"),
                cursor.to_string(),
            ),
            (format!("catalog_snapshot_v2_at:{chat}"), now.to_string()),
            (
                format!("catalog_snapshot_v2_message:{chat}"),
                message_id.to_string(),
            ),
        ] {
            transaction
                .execute(
                    "INSERT INTO app_meta(key,value) VALUES (?1,?2)
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![key, value],
                )
                .map_err(|e| e.to_string())?;
        }
        transaction.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    fn snapshot_message_id(&self, chat: i64) -> Result<Option<i64>, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let value: Option<String> = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [format!("catalog_snapshot_v2_message:{chat}")],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        value
            .map(|raw| raw.parse::<i64>().map_err(|e| e.to_string()))
            .transpose()
    }

    fn should_verify_deleted(&self, chat: i64, interval_seconds: i64) -> Result<bool, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let now: i64 = connection
            .query_row("SELECT unixepoch()", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        let key = format!("catalog_delete_verify_v2:{chat}");
        let previous: Option<String> = connection
            .query_row("SELECT value FROM app_meta WHERE key=?1", [&key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(previous) = previous else {
            // The new persistent delete-event protocol covers Nuvio-to-Nuvio deletes.
            // Seed the timer on upgrade instead of forcing thousands of getMessages
            // calls during the user's first fast sync. Direct Telegram deletions are
            // still caught by the periodic deep verification later.
            connection
                .execute(
                    "INSERT INTO app_meta(key,value) VALUES (?1,?2)",
                    params![key, now.to_string()],
                )
                .map_err(|e| e.to_string())?;
            return Ok(false);
        };
        let previous = previous.parse::<i64>().unwrap_or(0);
        Ok(now.saturating_sub(previous) >= interval_seconds.max(60))
    }

    fn mark_deleted_verified(&self, chat: i64) -> Result<(), String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let now: i64 = connection
            .query_row("SELECT unixepoch()", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        connection
            .execute(
                "INSERT INTO app_meta(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![format!("catalog_delete_verify_v2:{chat}"), now.to_string()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn trash_backfill_done(&self, chat: i64) -> Result<bool, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let found: Option<String> = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [format!("trash_sync_backfill_v1:{chat}")],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(found.as_deref() == Some("1"))
    }

    fn mark_trash_backfill_done(&self, chat: i64) -> Result<(), String> {
        self.connection.lock().map_err(|e| e.to_string())?.execute(
            "INSERT INTO app_meta(key,value) VALUES (?1,'1') ON CONFLICT(key) DO UPDATE SET value='1'",
            [format!("trash_sync_backfill_v1:{chat}")],
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn trashed_remote_message_ids(&self) -> Result<Vec<i64>, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT telegram_message_id FROM files WHERE provider='telegram' AND trashed=1 AND telegram_message_id IS NOT NULL ORDER BY rowid",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let mut ids = Vec::new();
        for row in rows {
            let raw = row.map_err(|e| e.to_string())?;
            ids.push(raw.parse::<i64>().map_err(|_| {
                "El catálogo contiene un identificador remoto inválido".to_string()
            })?);
        }
        Ok(ids)
    }

    fn catalog_message_ids(&self) -> Result<Vec<i64>, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT telegram_message_id FROM files WHERE provider='telegram' AND telegram_message_id IS NOT NULL ORDER BY rowid",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let mut ids = Vec::new();
        for row in rows {
            let raw = row.map_err(|e| e.to_string())?;
            ids.push(raw.parse::<i64>().map_err(|_| {
                "El catálogo contiene un identificador remoto inválido".to_string()
            })?);
        }
        Ok(ids)
    }
}

#[cfg(any(test, target_os = "android"))]
impl CatalogRepository {
    pub fn enqueue_android_downloads(
        &self,
        ids: &[String],
        tree: &str,
        policy: &str,
        existing: Vec<String>,
    ) -> Vec<BatchDownloadItem> {
        let mut occupied: std::collections::HashSet<String> = existing.into_iter().collect();
        ids.iter().map(|file_id| {
            let result = (|| {
                if !tree.starts_with("content://") || !matches!(policy, "skip" | "rename") {
                    return Err("Carpeta o política de Android inválida".to_string());
                }
                let doc = self.remote(file_id)?;
                validate_download_name(&doc.name)?;
                let mut c = self.connection.lock().map_err(|e| e.to_string())?;
                let tx = c.transaction().map_err(|e| e.to_string())?;
                {
                    let mut statement = tx.prepare("SELECT m.local_path FROM transfer_metadata m JOIN transfers t ON t.id=m.transfer_id WHERE t.direction='download' AND t.status NOT IN ('completed','cancelled')").map_err(|e| e.to_string())?;
                    let paths = statement.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
                    for path in paths {
                        let path = path.map_err(|e| e.to_string())?;
                        if let Some(data) = path.strip_prefix("nuvio-saf:") {
                            if let Ok(destination) = serde_json::from_str::<AndroidDestination>(data) {
                                if destination.tree == tree { occupied.insert(destination.name); }
                            }
                        }
                    }
                }
                let mut name = doc.name.clone();
                if occupied.contains(&name) {
                    if policy == "skip" {
                        return Ok(BatchDownloadItem { file_id: file_id.clone(), transfer_id: None, status: "skipped".into(), path: Some(name), message: "Omitido: ya existe o está reservado en la cola".into() });
                    }
                    let path = Path::new(&doc.name);
                    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("archivo");
                    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                    for index in 1_u64.. {
                        name = if extension.is_empty() { format!("{stem} ({index})") } else { format!("{stem} ({index}).{extension}") };
                        if !occupied.contains(&name) { break; }
                    }
                }
                let destination = AndroidDestination { tree: tree.into(), name: name.clone(), policy: policy.into() };
                let path = format!("nuvio-saf:{}", serde_json::to_string(&destination).map_err(|e| e.to_string())?);
                let id = insert_download(&tx, &doc, &path)?;
                tx.commit().map_err(|e| e.to_string())?;
                occupied.insert(name.clone());
                Ok(BatchDownloadItem { file_id: file_id.clone(), transfer_id: Some(id), status: "queued".into(), path: Some(name), message: "Añadido a la cola".into() })
            })();
            result.unwrap_or_else(|message| BatchDownloadItem { file_id: file_id.clone(), transfer_id: None, status: "error".into(), path: None, message })
        }).collect()
    }
}

impl CatalogRepository {
    fn apply_folder_snapshot(&self, mut events: Vec<FolderEvent>) -> Result<(), String> {
        events.sort_by(|a, b| a.id.cmp(&b.id));
        let mut connection = self.connection.lock().map_err(|e| e.to_string())?;
        let tx = connection.transaction().map_err(|e| e.to_string())?;
        // Replace links together. An old local hierarchy must not influence the
        // order in which a newer hierarchy from another device is reconstructed.
        for event in &events {
            crate::repository::validate_folder_name(&event.name).map_err(|e| e.to_string())?;
            tx.execute("INSERT INTO folders(id,name,parent_id,trashed) VALUES (?1,?2,NULL,?3) ON CONFLICT(id) DO UPDATE SET name=excluded.name,parent_id=NULL,trashed=excluded.trashed,updated_at=unixepoch()",
                params![event.id, event.name.trim(), event.trashed]).map_err(|e| e.to_string())?;
        }
        for event in &events {
            let parent = match crate::repository::ensure_folder_parent(
                &tx,
                event.parent_id.as_deref(),
                Some(&event.id),
            ) {
                Ok(()) => event.parent_id.as_deref(),
                Err(RepositoryError::Database(
                    rusqlite::Error::QueryReturnedNoRows | rusqlite::Error::InvalidParameterName(_),
                )) => None,
                Err(error) => return Err(error.to_string()),
            };
            tx.execute(
                "UPDATE folders SET parent_id=?1 WHERE id=?2",
                params![parent, event.id],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.execute("UPDATE file_locations SET folder_id=NULL WHERE folder_id IN (SELECT id FROM folders WHERE trashed=1)", []).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn reconcile_moves_and_orphans(
        &self,
        moves: &std::collections::HashMap<i64, Option<String>>,
    ) -> Result<(), String> {
        let mut connection = self.connection.lock().map_err(|e| e.to_string())?;
        let tx = connection.transaction().map_err(|e| e.to_string())?;

        if !moves.is_empty() {
            let valid_folders: std::collections::HashSet<String> = {
                let mut folders = tx
                    .prepare("SELECT id FROM folders WHERE trashed=0")
                    .map_err(|e| e.to_string())?;
                let rows = folders
                    .query_map([], |row| row.get::<_, String>(0))
                    .map_err(|e| e.to_string())?;
                let mut ids = std::collections::HashSet::new();
                for row in rows {
                    ids.insert(row.map_err(|e| e.to_string())?);
                }
                ids
            };
            let mut stmt = tx
                .prepare("UPDATE file_locations SET folder_id=?1 WHERE file_id=?2")
                .map_err(|e| e.to_string())?;
            for (&message_id, target_folder) in moves {
                let file_id = format!("tg-{message_id}");
                let valid_folder = target_folder
                    .as_deref()
                    .filter(|fid| valid_folders.contains(*fid));
                let _ = stmt.execute(params![valid_folder, file_id]);
            }
        }

        tx.execute(
            "UPDATE file_locations SET folder_id=NULL 
             WHERE folder_id IS NOT NULL 
               AND NOT EXISTS (SELECT 1 FROM folders WHERE folders.id = file_locations.folder_id AND folders.trashed = 0)",
            [],
        )
        .map_err(|e| e.to_string())?;

        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn reconcile_trash_states(
        &self,
        trash_states: &std::collections::HashMap<i64, bool>,
    ) -> Result<(), String> {
        if trash_states.is_empty() {
            return Ok(());
        }
        let mut connection = self.connection.lock().map_err(|e| e.to_string())?;
        let tx = connection.transaction().map_err(|e| e.to_string())?;
        let mut stmt = tx
            .prepare("UPDATE files SET trashed=?1 WHERE id=?2")
            .map_err(|e| e.to_string())?;
        for (&message_id, &trashed) in trash_states {
            let file_id = format!("tg-{message_id}");
            stmt.execute(params![if trashed { 1 } else { 0 }, file_id])
                .map_err(|e| e.to_string())?;
        }
        drop(stmt);
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn init_cloud(&self) -> Result<(), RepositoryError> {
        self.connection.lock().expect("catalog").execute_batch(
            "CREATE TABLE IF NOT EXISTS remote_documents (id TEXT PRIMARY KEY, document TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS transfer_pending (id TEXT PRIMARY KEY, message_id INTEGER NOT NULL);",
        )?;
        Ok(())
    }

    pub fn bind_account(&self, id: i64) -> Result<(), String> {
        let c = self.connection.lock().map_err(|e| e.to_string())?;
        let previous: Option<String> = c
            .query_row(
                "SELECT value FROM app_meta WHERE key='telegram_account'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if previous.is_some_and(|v| v != id.to_string()) {
            return Err("Esta biblioteca pertenece a otra cuenta de Telegram. Vuelve a conectar la cuenta original para proteger tus archivos.".into());
        }
        c.execute(
            "INSERT OR IGNORE INTO app_meta VALUES ('telegram_account',?1)",
            [id.to_string()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn bound_chat(&self) -> Result<Option<i64>, String> {
        let connection = self.connection.lock().map_err(|e| e.to_string())?;
        let value: Option<String> = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key='telegram_saved_messages_chat'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        value
            .map(|raw| raw.parse::<i64>().map_err(|e| e.to_string()))
            .transpose()
    }

    fn bind_chat(&self, chat_id: i64) -> Result<(), String> {
        self.connection
            .lock()
            .map_err(|e| e.to_string())?
            .execute(
                "INSERT INTO app_meta(key,value) VALUES ('telegram_saved_messages_chat',?1)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [chat_id.to_string()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn claim_pending(&self, direction: &str) -> Result<Option<WorkItem>, RepositoryError> {
        let mut c = self.connection.lock().expect("catalog");
        let tx = c.transaction()?;
        let wanted = if direction == "upload" {
            "ready"
        } else {
            "queued"
        };
        let row: Option<WorkItem> = tx
            .query_row(
                "SELECT t.id,m.local_path,m.size_bytes,m.sha256,t.direction,p.message_id,tf.folder_id
                 FROM transfers t
                 JOIN transfer_metadata m ON m.transfer_id=t.id
                 LEFT JOIN transfer_pending p ON p.id=t.id
                 LEFT JOIN transfer_folders tf ON tf.transfer_id=t.id
                 WHERE t.direction=?1 AND t.status=?2 AND m.encrypted=0
                 ORDER BY t.rowid LIMIT 1",
                params![direction, wanted],
                |r| {
                    Ok(WorkItem {
                        id: r.get(0)?,
                        path: r.get(1)?,
                        size: r.get(2)?,
                        sha256: r.get(3)?,
                        direction: r.get(4)?,
                        pending: r.get(5)?,
                        folder_id: r.get(6)?,
                    })
                },
            )
            .optional()?;
        if let Some(job) = &row {
            let active = if direction == "upload" {
                "uploading"
            } else {
                "downloading"
            };
            let label = if direction == "upload" {
                "Iniciando subida"
            } else {
                "Iniciando descarga"
            };
            tx.execute(
                "UPDATE transfers SET status=?1,speed_label=?2 WHERE id=?3 AND status=?4",
                params![active, label, job.id, wanted],
            )?;
            tx.execute(
                "UPDATE transfer_runtime SET phase=?1,processed_bytes=0,speed_bps=0,eta_seconds=NULL,pause_requested=0,cancel_requested=0,started_at=COALESCE(started_at,unixepoch()),updated_at=unixepoch() WHERE transfer_id=?2",
                params![active, job.id],
            )?;
        }
        tx.commit()?;
        Ok(row)
    }

    pub fn set_pending(&self, id: &str, message: i64) -> Result<(), RepositoryError> {
        self.connection.lock().expect("catalog").execute(
            "INSERT OR REPLACE INTO transfer_pending VALUES (?1,?2)",
            params![id, message],
        )?;
        Ok(())
    }

    pub fn clear_pending(&self, id: &str) -> Result<(), RepositoryError> {
        self.connection
            .lock()
            .expect("catalog")
            .execute("DELETE FROM transfer_pending WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn remote(&self, id: &str) -> Result<RemoteDocument, String> {
        let c = self.connection.lock().map_err(|e| e.to_string())?;
        let json: String = c
            .query_row(
                "SELECT document FROM remote_documents WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .map_err(|_| "El archivo todavía no tiene una copia confirmada en Telegram")?;
        serde_json::from_str(&json).map_err(|e| e.to_string())
    }

    pub fn cache_minithumbnail(&self, id: &str, data: &str) -> Result<(), String> {
        if data.is_empty() || data.len() > 64 * 1024 {
            return Ok(());
        }
        let mut c = self.connection.lock().map_err(|e| e.to_string())?;
        let tx = c.transaction().map_err(|e| e.to_string())?;
        let current: Option<String> = tx
            .query_row(
                "SELECT document FROM remote_documents WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(raw) = current {
            let mut doc: RemoteDocument = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            if doc.minithumbnail.as_deref() != Some(data) {
                doc.minithumbnail = Some(data.to_string());
                tx.execute(
                    "UPDATE remote_documents SET document=?1 WHERE id=?2",
                    params![serde_json::to_string(&doc).map_err(|e| e.to_string())?, id],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn record_remote(&self, doc: &RemoteDocument) -> Result<(), RepositoryError> {
        self.record_remote_batch(std::slice::from_ref(doc))
    }

    fn record_remote_batch(&self, documents: &[RemoteDocument]) -> Result<(), RepositoryError> {
        if documents.is_empty() {
            return Ok(());
        }
        let mut c = self.connection.lock().expect("catalog");
        let tx = c.transaction()?;
        {
            // Most documents discovered during sync were uploaded on another device or
            // belong to transfer history that has already been cleared locally. Resolve
            // only the transfer ids present in this <=100-item page, then skip five
            // guaranteed-no-op UPDATE/DELETE statements for every other remote row.
            let candidate_transfers: std::collections::HashSet<&str> = documents
                .iter()
                .filter_map(|doc| (!doc.transfer.is_empty()).then_some(doc.transfer.as_str()))
                .collect();
            let mut local_transfer_ids = std::collections::HashSet::new();
            if !candidate_transfers.is_empty() {
                let placeholders = std::iter::repeat_n("?", candidate_transfers.len())
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT id FROM transfers WHERE direction='upload' AND id IN ({placeholders})"
                );
                let mut statement = tx.prepare(&sql)?;
                let rows = statement.query_map(
                    rusqlite::params_from_iter(candidate_transfers.iter().copied()),
                    |row| row.get::<_, String>(0),
                )?;
                for row in rows {
                    local_transfer_ids.insert(row?);
                }
            }
            let active_folder_ids: std::collections::HashSet<String> = {
                let mut statement = tx.prepare_cached("SELECT id FROM folders WHERE trashed=0")?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                let mut ids = std::collections::HashSet::new();
                for row in rows {
                    ids.insert(row?);
                }
                ids
            };

            // Prepare once per batch instead of reparsing SQL statements for every
            // file. This matters on initial libraries with tens of thousands of docs.
            let mut file_stmt = tx.prepare_cached(
                "INSERT INTO files (id,name,extension,kind,size_bytes,updated_at,folder,provider,telegram_message_id)
                 VALUES (?1,?2,?3,?4,?5,strftime('%Y-%m-%dT%H:%M:%SZ',?6,'unixepoch'),'Mensajes guardados','telegram',?7)
                 ON CONFLICT(id) DO UPDATE SET name=excluded.name,extension=excluded.extension,kind=excluded.kind,size_bytes=excluded.size_bytes,updated_at=excluded.updated_at",
            )?;
            let mut location_stmt = tx.prepare_cached(
                "INSERT INTO file_locations(file_id,folder_id) VALUES (?1,?2)
                 ON CONFLICT(file_id) DO UPDATE SET folder_id=COALESCE(excluded.folder_id, file_locations.folder_id)",
            )?;
            let mut remote_stmt =
                tx.prepare_cached("INSERT OR REPLACE INTO remote_documents VALUES (?1,?2)")?;
            let mut transfer_stmt = tx.prepare_cached(
                "UPDATE transfers SET status='completed',progress=100,speed_label='Guardado en Telegram' WHERE id=?1 AND direction='upload'",
            )?;
            let mut metadata_stmt = tx.prepare_cached(
                "UPDATE transfer_metadata SET remote_message_id=?1,error=NULL WHERE transfer_id=?2",
            )?;
            let mut runtime_stmt = tx.prepare_cached(
                "UPDATE transfer_runtime SET phase='completed',processed_bytes=total_bytes,speed_bps=0,eta_seconds=0,updated_at=unixepoch(),completed_at=unixepoch() WHERE transfer_id=?1",
            )?;
            let mut cleanup_ledger_stmt = tx.prepare_cached(
                "INSERT INTO uploaded_sources(
                    transfer_id,file_name,source_path,size_bytes,sha256,remote_message_id,
                    source_deleted,source_delete_error,created_at
                 )
                 SELECT t.id,t.file_name,m.source_path,m.size_bytes,m.sha256,?1,
                        COALESCE(m.source_deleted,0),m.source_delete_error,m.created_at
                 FROM transfers t JOIN transfer_metadata m ON m.transfer_id=t.id
                 WHERE t.id=?2 AND t.direction='upload'
                   AND m.source_path IS NOT NULL AND m.source_path<>'' AND m.sha256<>''
                 ON CONFLICT(transfer_id) DO UPDATE SET
                    file_name=excluded.file_name,
                    source_path=excluded.source_path,
                    size_bytes=excluded.size_bytes,
                    sha256=excluded.sha256,
                    remote_message_id=excluded.remote_message_id,
                    source_deleted=MAX(uploaded_sources.source_deleted,excluded.source_deleted),
                    source_delete_error=CASE WHEN uploaded_sources.source_deleted=1 THEN NULL ELSE excluded.source_delete_error END",
            )?;
            let mut pending_stmt = tx.prepare_cached("DELETE FROM transfer_pending WHERE id=?1")?;

            for doc in documents {
                let id = format!("tg-{}", doc.message_id);
                let path = Path::new(&doc.name);
                let ext = path
                    .extension()
                    .and_then(|x| x.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                let name = path
                    .file_stem()
                    .and_then(|x| x.to_str())
                    .unwrap_or(&doc.name);
                let kind = classify_extension(&ext);
                file_stmt.execute(params![
                    id,
                    name,
                    ext,
                    kind,
                    doc.size,
                    doc.date,
                    doc.message_id.to_string()
                ])?;
                let valid_folder = doc
                    .folder_id
                    .as_deref()
                    .filter(|folder| active_folder_ids.contains(*folder));
                location_stmt.execute(params![id, valid_folder])?;
                remote_stmt.execute(params![id, serde_json::to_string(doc)?])?;
                if !doc.transfer.is_empty() && local_transfer_ids.contains(&doc.transfer) {
                    transfer_stmt.execute([&doc.transfer])?;
                    metadata_stmt.execute(params![doc.message_id.to_string(), doc.transfer])?;
                    runtime_stmt.execute([&doc.transfer])?;
                    cleanup_ledger_stmt
                        .execute(params![doc.message_id.to_string(), doc.transfer])?;
                    pending_stmt.execute([&doc.transfer])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn trash_many(&self, ids: &[String], trashed: bool) -> Result<usize, RepositoryError> {
        let mut connection = self.connection.lock().expect("catalog");
        let transaction = connection.transaction()?;
        let mut changed = 0usize;
        for id in ids {
            changed += transaction.execute(
                "UPDATE files SET trashed=?1 WHERE id=?2",
                params![trashed, id],
            )?;
        }
        transaction.commit()?;
        Ok(changed)
    }

    pub fn trashed_ids(&self) -> Result<Vec<String>, RepositoryError> {
        let connection = self.connection.lock().expect("catalog");
        let mut statement =
            connection.prepare("SELECT id FROM files WHERE trashed=1 ORDER BY updated_at")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    pub fn remove_catalog_files(&self, ids: &[String]) -> Result<usize, RepositoryError> {
        let mut connection = self.connection.lock().expect("catalog");
        let transaction = connection.transaction()?;
        let mut removed = 0usize;
        for id in ids {
            transaction.execute("DELETE FROM remote_documents WHERE id=?1", [id])?;
            removed += transaction.execute("DELETE FROM files WHERE id=?1", [id])?;
        }
        transaction.commit()?;
        Ok(removed)
    }

    fn enqueue_download_in_directory(
        &self,
        doc: &RemoteDocument,
        directory: &Path,
        policy: &str,
    ) -> Result<BatchDownloadItem, String> {
        validate_download_name(&doc.name)?;
        if policy != "skip" && policy != "rename" {
            return Err("Política de archivos existentes no válida".into());
        }
        let directory = directory.canonicalize().map_err(|e| e.to_string())?;
        if !directory.is_dir() {
            return Err("Elige una carpeta de destino válida".into());
        }
        // Hold the transaction until the name has been reserved by the new job.
        // Queued, paused and failed jobs reserve their targets even before a file exists.
        let mut c = self.connection.lock().map_err(|e| e.to_string())?;
        let tx = c.transaction().map_err(|e| e.to_string())?;
        let desired = directory.join(&doc.name);
        let mut target = desired.clone();
        if download_target_taken(&tx, &target)? {
            if policy == "skip" {
                return Ok(BatchDownloadItem {
                    file_id: format!("tg-{}", doc.message_id),
                    transfer_id: None,
                    status: "skipped".into(),
                    path: Some(target.to_string_lossy().into_owned()),
                    message: "Omitido: ya existe o está reservado en la cola".into(),
                });
            }
            let stem = desired
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("archivo");
            let ext = desired.extension().and_then(|s| s.to_str()).unwrap_or("");
            let mut index = 1_u64;
            loop {
                let name = if ext.is_empty() {
                    format!("{stem} ({index})")
                } else {
                    format!("{stem} ({index}).{ext}")
                };
                target = directory.join(name);
                if !download_target_taken(&tx, &target)? {
                    break;
                }
                index = index
                    .checked_add(1)
                    .ok_or("No se pudo reservar un nombre de archivo")?;
            }
        }
        let id = insert_download(&tx, doc, &target.to_string_lossy())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(BatchDownloadItem {
            file_id: format!("tg-{}", doc.message_id),
            transfer_id: Some(id),
            status: "queued".into(),
            path: Some(target.to_string_lossy().into_owned()),
            message: "Añadido a la cola".into(),
        })
    }

    pub fn enqueue_downloads(
        &self,
        file_ids: &[String],
        directory: &Path,
        policy: &str,
    ) -> Vec<BatchDownloadItem> {
        file_ids
            .iter()
            .map(|file_id| {
                self.remote(file_id)
                    .and_then(|doc| self.enqueue_download_in_directory(&doc, directory, policy))
                    .unwrap_or_else(|message| BatchDownloadItem {
                        file_id: file_id.clone(),
                        transfer_id: None,
                        status: "error".into(),
                        path: None,
                        message,
                    })
            })
            .collect()
    }
}

fn insert_download(
    c: &rusqlite::Transaction<'_>,
    doc: &RemoteDocument,
    destination: &str,
) -> Result<String, String> {
    let id = format!("download-{:x}", rand::random::<u64>());
    c.execute(
            "INSERT INTO transfers (id,file_name,direction,progress,status,speed_label) VALUES (?1,?2,'download',0,'queued','En cola')",
            params![id, doc.name],
        ).map_err(|e| e.to_string())?;
    c.execute(
            "INSERT INTO transfer_metadata (transfer_id,local_path,size_bytes,sha256,encrypted,created_at) VALUES (?1,?2,?3,?4,0,unixepoch())",
            params![id, destination, doc.size, doc.sha256],
        ).map_err(|e| e.to_string())?;
    c.execute(
            "INSERT INTO transfer_runtime (transfer_id,phase,total_bytes,updated_at) VALUES (?1,'queued',?2,unixepoch())",
            params![id, doc.size],
        ).map_err(|e| e.to_string())?;
    c.execute(
        "INSERT INTO transfer_pending VALUES (?1,?2)",
        params![id, doc.message_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(id)
}

fn download_target_taken(c: &rusqlite::Connection, path: &Path) -> Result<bool, String> {
    if path.symlink_metadata().is_ok() {
        return Ok(true);
    }
    let mut statement = c.prepare("SELECT m.local_path FROM transfer_metadata m JOIN transfers t ON t.id=m.transfer_id WHERE t.direction='download' AND t.status NOT IN ('completed','cancelled')").map_err(|e| e.to_string())?;
    let paths = statement
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    for reserved in paths {
        let reserved = reserved.map_err(|e| e.to_string())?;
        // Older catalog entries store ordinary paths, while new jobs use a
        // canonical directory (including the Windows extended-path prefix).
        let reserved_path = Path::new(&reserved);
        let reserved = reserved_path
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            .zip(reserved_path.file_name())
            .map(|(parent, name)| parent.join(name))
            .unwrap_or_else(|| reserved_path.to_path_buf());
        #[cfg(windows)]
        let matches =
            reserved.to_string_lossy().to_lowercase() == path.to_string_lossy().to_lowercase();
        #[cfg(not(windows))]
        let matches = reserved == path;
        if matches {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn classify_extension(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" | "png" | "webp" | "gif" | "heic" | "heif" | "bmp" | "tif" | "tiff"
        | "svg" | "avif" | "ico" | "raw" | "dng" | "cr2" | "nef" | "arw" => "image",
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" | "mpg" | "mpeg" | "3gp" | "mts"
        | "m2ts" | "wmv" | "flv" => "video",
        "mp3" | "m4a" | "wav" | "ogg" | "flac" | "aac" | "opus" | "wma" | "amr" | "aiff"
        | "alac" => "audio",
        "pdf" => "pdf",
        "doc" | "docx" | "odt" | "rtf" | "pages" => "document",
        "xls" | "xlsx" | "xlsm" | "xlsb" | "csv" | "tsv" | "ods" | "numbers" => "spreadsheet",
        "ppt" | "pptx" | "pps" | "ppsx" | "odp" | "key" => "presentation",
        "txt" | "md" | "markdown" | "log" | "ini" | "cfg" | "conf" | "yaml" | "yml" | "toml"
        | "json" | "xml" => "text",
        "js" | "jsx" | "ts" | "tsx" | "html" | "htm" | "css" | "scss" | "sass" | "less" | "py"
        | "rs" | "go" | "java" | "kt" | "kts" | "c" | "h" | "cpp" | "hpp" | "cs" | "php" | "rb"
        | "swift" | "dart" | "sh" | "bash" | "zsh" | "ps1" | "sql" | "vue" | "svelte" => "code",
        "zip" | "rar" | "7z" | "gz" | "gzip" | "bz2" | "xz" | "tar" | "tgz" | "tbz2" | "zst"
        | "lz" | "001" => "archive",
        "epub" | "mobi" | "azw" | "azw3" | "fb2" | "cbz" | "cbr" => "ebook",
        "db" | "sqlite" | "sqlite3" | "mdb" | "accdb" | "parquet" | "avro" => "database",
        "ttf" | "otf" | "woff" | "woff2" | "eot" => "font",
        "apk" | "aab" | "exe" | "msi" | "msix" | "appx" | "deb" | "rpm" | "pkg" | "dmg" | "jar"
        | "war" => "package",
        "obj" | "stl" | "fbx" | "glb" | "gltf" | "blend" | "dwg" | "dxf" | "step" | "stp"
        | "3mf" => "model",
        _ => "other",
    }
}

fn validate_download_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.ends_with(['.', ' '])
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
        })
    {
        return Err("El nombre remoto no es válido para descargar en esta carpeta".into());
    }
    #[cfg(windows)]
    {
        let stem = name
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end()
            .to_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || stem
                .strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|suffix| {
                    matches!(
                        suffix,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                })
        {
            return Err("El nombre remoto está reservado por Windows".into());
        }
    }
    Ok(())
}

fn document(message: &t::Message) -> Option<RemoteDocument> {
    if message.sending_state.is_some() {
        return None;
    }
    let e::MessageContent::MessageDocument(content) = &message.content else {
        return None;
    };
    let marker = content.caption.text.strip_prefix("#Nuvio1 ")?;
    let caption: Caption = serde_json::from_str(marker).ok()?;
    if caption.v != 1
        || caption.sha256.len() != 64
        || !caption.sha256.bytes().all(|v| v.is_ascii_hexdigit())
        || !caption.transfer.starts_with("transfer-")
    {
        return None;
    }
    Some(RemoteDocument {
        transfer: caption.transfer,
        message_id: message.id,
        name: content.document.file_name.clone(),
        size: content.document.document.size,
        date: message.date,
        sha256: caption.sha256,
        folder_id: caption.folder_id,
        minithumbnail: content
            .document
            .minithumbnail
            .as_ref()
            .map(|mini| mini.data.clone()),
    })
}

fn folder_event(message: &t::Message) -> Option<FolderEvent> {
    if message.sending_state.is_some() {
        return None;
    }
    let e::MessageContent::MessageText(content) = &message.content else {
        return None;
    };
    let payload = content.text.text.strip_prefix("#NuvioFolder1 ")?;
    let event: FolderEvent = serde_json::from_str(payload).ok()?;
    if event.v != 1
        || !event.id.starts_with("folder-")
        || event.parent_id.as_deref() == Some(event.id.as_str())
        || crate::repository::validate_folder_name(&event.name).is_err()
    {
        return None;
    }
    Some(event)
}

fn file_move_event(message: &t::Message) -> Option<FileMoveEvent> {
    if message.sending_state.is_some() {
        return None;
    }
    let e::MessageContent::MessageText(content) = &message.content else {
        return None;
    };
    let payload = content.text.text.strip_prefix("#NuvioMove1 ")?;
    let event: FileMoveEvent = serde_json::from_str(payload).ok()?;
    if event.v != 1 || event.message_ids.is_empty() || event.message_ids.len() > 100 {
        return None;
    }
    Some(event)
}

fn parse_file_trash_text(text: &str) -> Option<FileTrashEvent> {
    let payload = text.strip_prefix("#NuvioTrash1 ")?;
    let event: FileTrashEvent = serde_json::from_str(payload).ok()?;
    if event.v != 1 || event.message_ids.is_empty() || event.message_ids.len() > 100 {
        return None;
    }
    Some(event)
}

fn file_trash_event(message: &t::Message) -> Option<FileTrashEvent> {
    if message.sending_state.is_some() {
        return None;
    }
    let e::MessageContent::MessageText(content) = &message.content else {
        return None;
    };
    parse_file_trash_text(&content.text.text)
}

fn parse_file_delete_text(text: &str) -> Option<FileDeleteEvent> {
    let payload = text.strip_prefix("#NuvioDelete1 ")?;
    let event: FileDeleteEvent = serde_json::from_str(payload).ok()?;
    if event.v != 1 || event.message_ids.is_empty() || event.message_ids.len() > 100 {
        return None;
    }
    Some(event)
}

fn file_delete_event(message: &t::Message) -> Option<FileDeleteEvent> {
    if message.sending_state.is_some() {
        return None;
    }
    let e::MessageContent::MessageText(content) = &message.content else {
        return None;
    };
    parse_file_delete_text(&content.text.text)
}

impl TelegramService {
    pub fn request_sync_cancel(&self) -> bool {
        let active = self
            .sync_progress
            .lock()
            .map(|progress| progress.active)
            .unwrap_or(false);
        if active {
            self.sync_cancel_requested.store(true, Ordering::Release);
        }
        active
    }

    fn reset_sync_cancel(&self) {
        self.sync_cancel_requested.store(false, Ordering::Release);
    }

    fn ensure_sync_not_cancelled(&self) -> Result<(), String> {
        if self.sync_cancel_requested.load(Ordering::Acquire) {
            Err(SYNC_CANCELLED_ERROR.to_string())
        } else {
            Ok(())
        }
    }

    async fn wait_sync_cancel(&self) {
        while !self.sync_cancel_requested.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn sync_call<T>(
        &self,
        request: impl std::future::Future<Output = Result<T, tdlib_rs::types::Error>>,
    ) -> Result<T, String> {
        tokio::select! {
            result = call(request) => result,
            _ = self.wait_sync_cancel() => Err(SYNC_CANCELLED_ERROR.to_string()),
        }
    }

    pub async fn own_chat(&self, repo: &CatalogRepository) -> Result<i64, String> {
        if !self.refresh().await?.connected {
            return Err("Conecta tu cuenta de Telegram primero".into());
        }
        let e::User::User(me) = call(f::get_me(self.client_id())).await?;
        repo.bind_account(me.id)?;
        let e::Chat::Chat(chat) =
            call(f::create_private_chat(me.id, false, self.client_id())).await?;
        repo.bind_chat(chat.id)?;
        Ok(chat.id)
    }

    async fn latest_catalog_snapshot(
        &self,
        chat: i64,
    ) -> Result<Option<(i64, CatalogSnapshotCaption, t::File)>, String> {
        self.ensure_sync_not_cancelled()?;
        let e::FoundChatMessages::FoundChatMessages(page) = self
            .sync_call(f::search_chat_messages(
                chat,
                None,
                "#NuvioCatalog2".to_string(),
                None,
                0,
                0,
                50,
                None,
                self.client_id(),
            ))
            .await?;

        let mut best: Option<(i64, CatalogSnapshotCaption, t::File)> = None;
        for message in page.messages {
            if message.chat_id != chat {
                continue;
            }
            let Some((caption, file)) = catalog_snapshot_message(&message) else {
                continue;
            };
            let replace = best.as_ref().is_none_or(|(best_id, best_caption, _)| {
                caption.generated_at > best_caption.generated_at
                    || (caption.generated_at == best_caption.generated_at && message.id > *best_id)
            });
            if replace {
                best = Some((message.id, caption, file));
            }
        }
        Ok(best)
    }

    async fn download_catalog_snapshot(
        &self,
        caption: &CatalogSnapshotCaption,
        mut file: t::File,
    ) -> Result<CatalogSnapshotV2, String> {
        const MAX_COMPRESSED_BYTES: i64 = 512 * 1024 * 1024;
        let advertised_size = file.size.max(file.expected_size);
        if !(0..=MAX_COMPRESSED_BYTES).contains(&advertised_size) {
            return Err("El snapshot remoto excede el límite de seguridad".into());
        }

        self.ensure_sync_not_cancelled()?;
        let mut updates = self.subscribe_file_updates();
        let e::File::File(started) = self
            .sync_call(f::download_file(file.id, 32, 0, 0, false, self.client_id()))
            .await?;
        file = started;

        let deadline = Instant::now() + Duration::from_secs(20 * 60);
        while !file.local.is_downloading_completed {
            self.ensure_sync_not_cancelled()?;
            if Instant::now() >= deadline {
                return Err("El snapshot remoto tardó demasiado en descargarse".into());
            }
            if !file.local.is_downloading_active {
                return Err("Telegram interrumpió la descarga del snapshot".into());
            }
            file = self
                .next_file_update(&mut updates, file.id, Duration::from_millis(500))
                .await?;
        }

        self.ensure_sync_not_cancelled()?;
        let path = PathBuf::from(&file.local.path);
        let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_COMPRESSED_BYTES as u64 {
            return Err("El snapshot descargado no es un archivo válido".into());
        }

        let hash_path = path.clone();
        let actual_hash = tauri::async_runtime::spawn_blocking(move || sha256_file(&hash_path))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        self.ensure_sync_not_cancelled()?;
        if !actual_hash.eq_ignore_ascii_case(&caption.sha256) {
            return Err("El hash del snapshot remoto no coincide".into());
        }

        let snapshot_path = path.clone();
        let snapshot =
            tauri::async_runtime::spawn_blocking(move || read_catalog_snapshot_zip(&snapshot_path))
                .await
                .map_err(|e| e.to_string())??;
        self.ensure_sync_not_cancelled()?;
        if snapshot.documents.len() != caption.documents
            || snapshot.generated_at != caption.generated_at
        {
            return Err("Los metadatos del snapshot remoto no coinciden con su contenido".into());
        }
        Ok(snapshot)
    }

    async fn import_latest_catalog_snapshot(
        &self,
        repo: &CatalogRepository,
        chat: i64,
    ) -> Result<Option<usize>, String> {
        if repo.catalog_remote_count()? != 0 {
            return Ok(None);
        }
        let Some((message_id, caption, file)) = self.latest_catalog_snapshot(chat).await? else {
            return Ok(None);
        };

        // A malformed or stale snapshot must never block the normal history fallback.
        // Explicit user cancellation is different: propagate it immediately instead of
        // silently falling back to a potentially very long history scan.
        let snapshot = match self.download_catalog_snapshot(&caption, file).await {
            Ok(snapshot) => snapshot,
            Err(error) if error == SYNC_CANCELLED_ERROR => return Err(error),
            Err(_) => return Ok(None),
        };
        let imported = repo.import_catalog_snapshot(chat, &snapshot)?;
        let cursor = [
            snapshot.checkpoints.documents,
            snapshot.checkpoints.folders,
            snapshot.checkpoints.moves,
            snapshot.checkpoints.trash,
            snapshot.checkpoints.deletes,
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        repo.mark_snapshot_published(chat, cursor, message_id)?;
        Ok(Some(imported))
    }

    async fn wait_sent_message(
        &self,
        chat: i64,
        message: t::Message,
        timeout_for: Duration,
        timeout_message: &str,
    ) -> Result<t::Message, String> {
        if message.sending_state.is_none() {
            return Ok(message);
        }
        let pending = message.id;
        let deadline = Instant::now() + timeout_for;
        while Instant::now() < deadline {
            if self.sync_cancel_requested.load(Ordering::Acquire) {
                // This helper is used only by catalog snapshot publication. Deleting
                // the pending message asks TDLib to stop the upload instead of leaving
                // a hidden snapshot transfer consuming bandwidth after the user stops sync.
                let _ = call(f::delete_messages(
                    chat,
                    vec![pending],
                    true,
                    self.client_id(),
                ))
                .await;
                return Err(SYNC_CANCELLED_ERROR.to_string());
            }
            if let Some(result) = self
                .sent
                .lock()
                .map_err(|e| e.to_string())?
                .remove(&pending)
            {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
        Err(timeout_message.to_string())
    }

    async fn publish_catalog_snapshot_if_needed(
        &self,
        repo: &CatalogRepository,
        chat: i64,
        cursor: i64,
    ) -> Result<(), String> {
        if !repo.snapshot_publish_needed(chat, cursor)? {
            return Ok(());
        }

        self.ensure_sync_not_cancelled()?;
        let snapshot = repo.export_catalog_snapshot(chat)?;
        self.ensure_sync_not_cancelled()?;
        let documents = snapshot.documents.len();
        let generated_at = snapshot.generated_at;
        let temp = tempfile::Builder::new()
            .prefix("nuvio-catalog-v2-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let path = temp.path().join("nuvio-catalog-v2.zip");
        let build_path = path.clone();
        let hash = tauri::async_runtime::spawn_blocking(move || {
            write_catalog_snapshot_zip(&snapshot, &build_path)?;
            sha256_file(&build_path).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())??;
        self.ensure_sync_not_cancelled()?;
        let caption = CatalogSnapshotCaption {
            v: 2,
            sha256: hash,
            documents,
            generated_at,
        };
        let text = format!(
            "#NuvioCatalog2 {}",
            serde_json::to_string(&caption).map_err(|e| e.to_string())?
        );
        let content = e::InputMessageContent::InputMessageDocument(t::InputMessageDocument {
            document: e::InputFile::Local(t::InputFileLocal {
                path: path.to_string_lossy().into_owned(),
            }),
            thumbnail: None,
            disable_content_type_detection: true,
            caption: Some(t::FormattedText {
                text,
                entities: vec![],
            }),
        });

        self.ensure_sync_not_cancelled()?;
        let old_message = repo.snapshot_message_id(chat)?;
        let e::Message::Message(message) = self
            .sync_call(f::send_message(
                chat,
                None,
                None,
                None,
                content,
                self.client_id(),
            ))
            .await?;
        let message = self
            .wait_sent_message(
                chat,
                message,
                Duration::from_secs(15 * 60),
                "Telegram tardó demasiado en publicar el snapshot del catálogo",
            )
            .await?;
        repo.mark_snapshot_published(chat, cursor, message.id)?;

        if let Some(old_id) = old_message.filter(|old_id| *old_id > 0 && *old_id != message.id) {
            let _ = call(f::delete_messages(
                chat,
                vec![old_id],
                true,
                self.client_id(),
            ))
            .await;
        }
        Ok(())
    }

    pub async fn apply_realtime_updates(&self, repo: &CatalogRepository) -> Result<usize, String> {
        let Ok(_gate) = self.catalog_sync_gate.try_lock() else {
            return Ok(0);
        };
        let updates = self.drain_realtime_updates(512);
        if updates.is_empty() {
            return Ok(0);
        }
        let needs_chat = updates.iter().any(|update| {
            matches!(
                update,
                TelegramRealtimeUpdate::Message(_) | TelegramRealtimeUpdate::DeleteMessages { .. }
            )
        });
        let chat = if needs_chat {
            Some(match repo.bound_chat()? {
                Some(chat) => chat,
                None => self.own_chat(repo).await?,
            })
        } else {
            None
        };

        let mut applied = 0usize;
        for update in updates {
            match update {
                TelegramRealtimeUpdate::Message(message) => {
                    let message = *message;
                    if Some(message.chat_id) != chat {
                        continue;
                    }
                    if let Some(doc) = document(&message) {
                        repo.record_remote(&doc).map_err(|e| e.to_string())?;
                        applied += 1;
                        continue;
                    }
                    if let Some(event) = folder_event(&message) {
                        repo.apply_folder_event(
                            &event.id,
                            &event.name,
                            event.parent_id.as_deref(),
                            event.trashed,
                        )
                        .map_err(|e| e.to_string())?;
                        applied += 1;
                        continue;
                    }
                    if let Some(event) = file_move_event(&message) {
                        let moves = event
                            .message_ids
                            .into_iter()
                            .map(|message_id| (message_id, event.folder_id.clone()))
                            .collect::<std::collections::HashMap<_, _>>();
                        repo.reconcile_moves_and_orphans(&moves)?;
                        applied += moves.len();
                        continue;
                    }
                    if let Some(event) = file_trash_event(&message) {
                        let states = event
                            .message_ids
                            .into_iter()
                            .map(|message_id| (message_id, event.trashed))
                            .collect::<std::collections::HashMap<_, _>>();
                        repo.reconcile_trash_states(&states)?;
                        applied += states.len();
                        continue;
                    }
                    if let Some(event) = file_delete_event(&message) {
                        let ids = event
                            .message_ids
                            .into_iter()
                            .map(|message_id| format!("tg-{message_id}"))
                            .collect::<Vec<_>>();
                        applied += repo.remove_catalog_files(&ids).map_err(|e| e.to_string())?;
                    }
                }
                TelegramRealtimeUpdate::DeleteMessages {
                    chat_id,
                    message_ids,
                    is_permanent,
                    from_cache,
                } => {
                    if Some(chat_id) != chat || !is_permanent || from_cache {
                        continue;
                    }
                    let ids = message_ids
                        .into_iter()
                        .map(|message_id| format!("tg-{message_id}"))
                        .collect::<Vec<_>>();
                    applied += repo.remove_catalog_files(&ids).map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(applied)
    }

    async fn send_metadata_text(&self, chat: i64, text: String) -> Result<(), String> {
        self.send_metadata_text_inner(chat, text, false).await
    }

    async fn send_sync_metadata_text(&self, chat: i64, text: String) -> Result<(), String> {
        self.send_metadata_text_inner(chat, text, true).await
    }

    async fn send_metadata_text_inner(
        &self,
        chat: i64,
        text: String,
        cancellable: bool,
    ) -> Result<(), String> {
        if cancellable {
            self.ensure_sync_not_cancelled()?;
        }
        let content = e::InputMessageContent::InputMessageText(t::InputMessageText {
            text: t::FormattedText {
                text,
                entities: vec![],
            },
            link_preview_options: None,
            clear_draft: false,
        });
        let request = f::send_message(chat, None, None, None, content, self.client_id());
        let response = if cancellable {
            self.sync_call(request).await?
        } else {
            call(request).await?
        };
        let e::Message::Message(message) = response;
        if message.sending_state.is_none() {
            return Ok(());
        }
        let pending = message.id;
        let deadline = Instant::now() + Duration::from_secs(25);
        while Instant::now() < deadline {
            if cancellable {
                self.ensure_sync_not_cancelled()?;
            }
            if let Some(result) = self
                .sent
                .lock()
                .map_err(|e| e.to_string())?
                .remove(&pending)
            {
                return result.map(|_| ());
            }
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
        Err("Telegram tardó demasiado en confirmar los metadatos de Nuvio".into())
    }

    pub async fn create_folder_synced(
        &self,
        repo: &CatalogRepository,
        name: &str,
        parent_id: Option<&str>,
    ) -> Result<String, String> {
        repo.validate_new_folder(name, parent_id)
            .map_err(|e| e.to_string())?;
        let id = format!("folder-{:016x}", rand::random::<u64>());
        let event = FolderEvent {
            v: 1,
            id: id.clone(),
            name: name.trim().to_string(),
            parent_id: parent_id.map(str::to_string),
            trashed: false,
        };
        let chat = self.own_chat(repo).await?;
        self.send_metadata_text(
            chat,
            format!(
                "#NuvioFolder1 {}",
                serde_json::to_string(&event).map_err(|e| e.to_string())?
            ),
        )
        .await?;
        repo.apply_folder_event(&id, &event.name, event.parent_id.as_deref(), false)
            .map_err(|e| e.to_string())?;
        Ok(id)
    }

    pub async fn update_folder_synced(
        &self,
        repo: &CatalogRepository,
        id: &str,
        name: &str,
        parent_id: Option<&str>,
        trashed: bool,
    ) -> Result<(), String> {
        repo.validate_folder_update(id, name, parent_id)
            .map_err(|e| e.to_string())?;
        if trashed && !repo.folder_is_empty(id).map_err(|e| e.to_string())? {
            return Err("La carpeta debe estar vacía antes de eliminarla".into());
        }
        let event = FolderEvent {
            v: 1,
            id: id.to_string(),
            name: name.trim().to_string(),
            parent_id: parent_id.map(str::to_string),
            trashed,
        };
        let chat = self.own_chat(repo).await?;
        self.send_metadata_text(
            chat,
            format!(
                "#NuvioFolder1 {}",
                serde_json::to_string(&event).map_err(|e| e.to_string())?
            ),
        )
        .await?;
        repo.update_folder_local(id, &event.name, event.parent_id.as_deref(), trashed)
            .map_err(|e| e.to_string())
    }

    pub async fn move_files_synced(
        &self,
        repo: &CatalogRepository,
        ids: &[String],
        folder_id: Option<&str>,
    ) -> Result<usize, String> {
        if ids.is_empty() {
            return Ok(0);
        }
        if ids.len() > 500 {
            return Err("Mueve como máximo 500 archivos por operación".into());
        }
        if let Some(folder) = folder_id {
            let target = repo.folder_by_id(folder).map_err(|e| e.to_string())?;
            if target.trashed {
                return Err("No puedes mover archivos a una carpeta eliminada".into());
            }
        }
        let chat = self.own_chat(repo).await?;
        let mut changed = 0usize;
        for chunk in ids.chunks(100) {
            let mut message_ids = Vec::with_capacity(chunk.len());
            let mut file_ids = Vec::with_capacity(chunk.len());
            for id in chunk {
                let doc = repo.remote(id)?;
                message_ids.push(doc.message_id);
                file_ids.push(id.clone());
            }
            let event = FileMoveEvent {
                v: 1,
                message_ids,
                folder_id: folder_id.map(str::to_string),
            };
            self.send_metadata_text(
                chat,
                format!(
                    "#NuvioMove1 {}",
                    serde_json::to_string(&event).map_err(|e| e.to_string())?
                ),
            )
            .await?;
            changed += repo
                .set_file_folder_local(&file_ids, folder_id)
                .map_err(|e| e.to_string())?;
        }
        Ok(changed)
    }

    pub async fn set_files_trashed_synced(
        &self,
        repo: &CatalogRepository,
        ids: &[String],
        trashed: bool,
    ) -> Result<usize, String> {
        if ids.is_empty() {
            return Ok(0);
        }
        if ids.len() > 500 {
            return Err("Selecciona como máximo 500 archivos por operación".into());
        }
        let chat = self.own_chat(repo).await?;
        let mut changed = 0usize;
        for chunk in ids.chunks(100) {
            let mut message_ids = Vec::with_capacity(chunk.len());
            for id in chunk {
                message_ids.push(repo.remote(id)?.message_id);
            }
            let event = FileTrashEvent {
                v: 1,
                message_ids,
                trashed,
            };
            self.send_metadata_text(
                chat,
                format!(
                    "#NuvioTrash1 {}",
                    serde_json::to_string(&event).map_err(|e| e.to_string())?
                ),
            )
            .await?;
            changed += repo.trash_many(chunk, trashed).map_err(|e| e.to_string())?;
        }
        Ok(changed)
    }

    async fn folder_events_since(
        &self,
        chat: i64,
        checkpoint: Option<i64>,
    ) -> Result<(std::collections::HashMap<String, FolderEvent>, i64), String> {
        let mut events = std::collections::HashMap::new();
        let mut newest = checkpoint.unwrap_or(0);
        let mut from_message_id = 0;
        loop {
            self.ensure_sync_not_cancelled()?;
            let e::FoundChatMessages::FoundChatMessages(page) = self
                .sync_call(f::search_chat_messages(
                    chat,
                    None,
                    "#NuvioFolder1".to_string(),
                    None,
                    from_message_id,
                    0,
                    100,
                    None,
                    self.client_id(),
                ))
                .await?;
            let next_from_message_id = page.next_from_message_id;
            let mut reached_checkpoint = false;
            for message in page.messages {
                if checkpoint.is_some_and(|saved| message.id <= saved) {
                    reached_checkpoint = true;
                    break;
                }
                newest = newest.max(message.id);
                if let Some(event) = folder_event(&message) {
                    events.entry(event.id.clone()).or_insert(event);
                }
            }
            if reached_checkpoint
                || next_from_message_id == 0
                || next_from_message_id == from_message_id
            {
                break;
            }
            from_message_id = next_from_message_id;
        }
        Ok((events, newest))
    }

    async fn file_move_events_since(
        &self,
        chat: i64,
        checkpoint: Option<i64>,
    ) -> Result<(std::collections::HashMap<i64, Option<String>>, i64), String> {
        let mut moves = std::collections::HashMap::new();
        let mut newest = checkpoint.unwrap_or(0);
        let mut from_message_id = 0;
        loop {
            self.ensure_sync_not_cancelled()?;
            let e::FoundChatMessages::FoundChatMessages(page) = self
                .sync_call(f::search_chat_messages(
                    chat,
                    None,
                    "#NuvioMove1".to_string(),
                    None,
                    from_message_id,
                    0,
                    100,
                    None,
                    self.client_id(),
                ))
                .await?;
            let next_from_message_id = page.next_from_message_id;
            let mut reached_checkpoint = false;
            for message in page.messages {
                if checkpoint.is_some_and(|saved| message.id <= saved) {
                    reached_checkpoint = true;
                    break;
                }
                newest = newest.max(message.id);
                if let Some(event) = file_move_event(&message) {
                    for id in event.message_ids {
                        moves.entry(id).or_insert_with(|| event.folder_id.clone());
                    }
                }
            }
            if reached_checkpoint
                || next_from_message_id == 0
                || next_from_message_id == from_message_id
            {
                break;
            }
            from_message_id = next_from_message_id;
        }
        Ok((moves, newest))
    }

    async fn file_trash_events_since(
        &self,
        chat: i64,
        checkpoint: Option<i64>,
    ) -> Result<(std::collections::HashMap<i64, bool>, i64), String> {
        let mut states = std::collections::HashMap::new();
        let mut newest = checkpoint.unwrap_or(0);
        let mut from_message_id = 0;
        loop {
            self.ensure_sync_not_cancelled()?;
            let e::FoundChatMessages::FoundChatMessages(page) = self
                .sync_call(f::search_chat_messages(
                    chat,
                    None,
                    "#NuvioTrash1".to_string(),
                    None,
                    from_message_id,
                    0,
                    100,
                    None,
                    self.client_id(),
                ))
                .await?;
            let next_from_message_id = page.next_from_message_id;
            let mut reached_checkpoint = false;
            for message in page.messages {
                if checkpoint.is_some_and(|saved| message.id <= saved) {
                    reached_checkpoint = true;
                    break;
                }
                newest = newest.max(message.id);
                if let Some(event) = file_trash_event(&message) {
                    for id in event.message_ids {
                        states.entry(id).or_insert(event.trashed);
                    }
                }
            }
            if reached_checkpoint
                || next_from_message_id == 0
                || next_from_message_id == from_message_id
            {
                break;
            }
            from_message_id = next_from_message_id;
        }
        Ok((states, newest))
    }

    async fn file_delete_events_since(
        &self,
        chat: i64,
        checkpoint: Option<i64>,
    ) -> Result<(std::collections::HashSet<i64>, i64), String> {
        let mut deleted = std::collections::HashSet::new();
        let mut newest = checkpoint.unwrap_or(0);
        let mut from_message_id = 0;
        loop {
            self.ensure_sync_not_cancelled()?;
            let e::FoundChatMessages::FoundChatMessages(page) = self
                .sync_call(f::search_chat_messages(
                    chat,
                    None,
                    "#NuvioDelete1".to_string(),
                    None,
                    from_message_id,
                    0,
                    100,
                    None,
                    self.client_id(),
                ))
                .await?;
            let next_from_message_id = page.next_from_message_id;
            let mut reached_checkpoint = false;
            for message in page.messages {
                if checkpoint.is_some_and(|saved| message.id <= saved) {
                    reached_checkpoint = true;
                    break;
                }
                newest = newest.max(message.id);
                if let Some(event) = file_delete_event(&message) {
                    deleted.extend(event.message_ids);
                }
            }
            if reached_checkpoint
                || next_from_message_id == 0
                || next_from_message_id == from_message_id
            {
                break;
            }
            from_message_id = next_from_message_id;
        }
        Ok((deleted, newest))
    }

    async fn scan_documents_fast(
        &self,
        repo: &CatalogRepository,
        progress: &crate::progress::SyncRun<'_>,
        chat: i64,
        state: &mut HistoryScanState,
    ) -> Result<FastSearchOutcome, String> {
        let mut from_message_id = 0;
        let mut pages = 0usize;
        let mut outcome = FastSearchOutcome {
            total_hint: -1,
            ..FastSearchOutcome::default()
        };
        // Publish every received Telegram page immediately. This keeps the UI flowing
        // even if the next network request stalls, while WAL/NORMAL keeps the 100-row
        // SQLite transactions cheap enough for large catalogs.
        let mut pending_documents = Vec::with_capacity(100);
        let mut pending_trash_states = std::collections::HashMap::new();

        loop {
            self.ensure_sync_not_cancelled()?;
            let e::FoundChatMessages::FoundChatMessages(page) = self
                .sync_call(f::search_chat_messages(
                    chat,
                    None,
                    "#Nuvio1".to_string(),
                    None,
                    from_message_id,
                    0,
                    100,
                    None,
                    self.client_id(),
                ))
                .await?;

            outcome.total_hint = outcome.total_hint.max(page.total_count);
            let next_from_message_id = page.next_from_message_id;

            for message in page.messages {
                let Some(mut doc) = document(&message) else {
                    continue;
                };
                outcome.fetched += 1;
                outcome.oldest_message_id = Some(
                    outcome
                        .oldest_message_id
                        .map_or(doc.message_id, |oldest| oldest.min(doc.message_id)),
                );
                state.newest = state.newest.max(doc.message_id);
                state.scanned += 1;

                if state.deleted_message_ids.contains(&doc.message_id) {
                    continue;
                }
                if let Some(folder) = state.moves.get(&doc.message_id) {
                    doc.folder_id = folder.clone();
                } else if doc.folder_id.is_some() {
                    state.moves.insert(doc.message_id, doc.folder_id.clone());
                }
                if let Some(&trashed) = state.trash_states.get(&doc.message_id) {
                    pending_trash_states.insert(doc.message_id, trashed);
                }
                pending_documents.push(doc);
            }

            if !pending_documents.is_empty() {
                state.total_documents += pending_documents.len();
                repo.record_remote_batch(&pending_documents)
                    .map_err(|e| e.to_string())?;
                if !pending_trash_states.is_empty() {
                    repo.reconcile_trash_states(&pending_trash_states)?;
                }
                pending_documents.clear();
                pending_trash_states.clear();
            }

            progress.scanned(state.scanned, None);
            pages += 1;
            if pages == 1 || pages.is_multiple_of(10) {
                Self::publish_sync_notification(
                    true,
                    crate::progress::SyncPhase::Files,
                    state.scanned,
                    None,
                    None,
                    None,
                );
            }

            if next_from_message_id == 0 {
                outcome.exhausted = true;
                break;
            }
            if next_from_message_id == from_message_id {
                outcome.stalled = true;
                break;
            }
            from_message_id = next_from_message_id;
            tokio::task::yield_now().await;
        }

        if !pending_documents.is_empty() {
            state.total_documents += pending_documents.len();
            repo.record_remote_batch(&pending_documents)
                .map_err(|e| e.to_string())?;
            if !pending_trash_states.is_empty() {
                repo.reconcile_trash_states(&pending_trash_states)?;
            }
        }
        Ok(outcome)
    }

    async fn scan_history_range(
        &self,
        repo: &CatalogRepository,
        progress: &crate::progress::SyncRun<'_>,
        chat: i64,
        start_cursor: i64,
        stop_at: Option<i64>,
        state: &mut HistoryScanState,
    ) -> Result<(), String> {
        let mut cursor = start_cursor;
        let mut pages = 0usize;
        // History pages can contain non-Nuvio metadata, but any Nuvio documents that
        // arrive are committed before requesting the next page so already-received
        // data is never hidden behind a slow Telegram response.
        let mut pending_documents = Vec::with_capacity(100);
        let mut pending_trash_states = std::collections::HashMap::new();
        loop {
            self.ensure_sync_not_cancelled()?;
            let e::Messages::Messages(page) = self
                .sync_call(f::get_chat_history(
                    chat,
                    cursor,
                    0,
                    100,
                    false,
                    self.client_id(),
                ))
                .await?;
            let mut last = cursor;
            let mut reached_checkpoint = false;
            let mut folders_changed = false;

            for msg in page.messages.into_iter().flatten() {
                if msg.id == cursor {
                    continue;
                }
                if stop_at.is_some_and(|saved| msg.id <= saved) {
                    reached_checkpoint = true;
                    break;
                }
                last = msg.id;
                state.newest = state.newest.max(msg.id);

                if let Some(event) = folder_event(&msg) {
                    if let std::collections::hash_map::Entry::Vacant(entry) =
                        state.folders.entry(event.id.clone())
                    {
                        entry.insert(event);
                        folders_changed = true;
                    }
                } else if let Some(event) = file_move_event(&msg) {
                    for id in event.message_ids {
                        state
                            .moves
                            .entry(id)
                            .or_insert_with(|| event.folder_id.clone());
                    }
                } else if let Some(event) = file_trash_event(&msg) {
                    for id in event.message_ids {
                        state.trash_states.entry(id).or_insert(event.trashed);
                    }
                } else if let Some(event) = file_delete_event(&msg) {
                    state.deleted_message_ids.extend(event.message_ids);
                } else if let Some(mut doc) = document(&msg) {
                    state.scanned += 1;
                    if state.deleted_message_ids.contains(&doc.message_id) {
                        continue;
                    }
                    if let Some(folder) = state.moves.get(&doc.message_id) {
                        doc.folder_id = folder.clone();
                    } else if doc.folder_id.is_some() {
                        // Preserve the upload-time folder as the fallback location. A
                        // newer #NuvioMove1 event always wins because it was inserted
                        // into `moves` first while scanning newest-to-oldest.
                        state.moves.insert(doc.message_id, doc.folder_id.clone());
                    }
                    if let Some(&trashed) = state.trash_states.get(&doc.message_id) {
                        pending_trash_states.insert(doc.message_id, trashed);
                    }
                    pending_documents.push(doc);
                }
            }

            if folders_changed {
                repo.apply_folder_snapshot(state.folders.values().cloned().collect())?;
            }
            if !pending_documents.is_empty() {
                state.total_documents += pending_documents.len();
                repo.record_remote_batch(&pending_documents)
                    .map_err(|e| e.to_string())?;
                if !pending_trash_states.is_empty() {
                    repo.reconcile_trash_states(&pending_trash_states)?;
                }
                pending_documents.clear();
                pending_trash_states.clear();
            }

            progress.scanned(state.scanned, None);
            pages += 1;
            // System notifications cross the Android bridge. Throttle them to one
            // update per ~500 discovered Nuvio files; the in-app progress state still
            // updates every page without this IPC overhead.
            if pages == 1 || pages.is_multiple_of(5) {
                Self::publish_sync_notification(
                    true,
                    crate::progress::SyncPhase::Files,
                    state.scanned,
                    None,
                    None,
                    None,
                );
            }
            if reached_checkpoint || last == cursor {
                break;
            }
            cursor = last;
            tokio::task::yield_now().await;
        }
        Ok(())
    }

    pub async fn sync_catalog(&self, repo: &CatalogRepository) -> Result<usize, String> {
        self.sync_catalog_checked(repo, false).await
    }

    pub async fn bootstrap_catalog_if_needed(
        &self,
        repo: &CatalogRepository,
    ) -> Result<bool, String> {
        let chat = self.own_chat(repo).await?;
        if !repo.initial_catalog_sync_needed(chat)? {
            return Ok(false);
        }
        self.sync_catalog(repo).await?;
        Ok(true)
    }

    fn publish_sync_notification(
        active: bool,
        phase: crate::progress::SyncPhase,
        scanned: usize,
        total: Option<usize>,
        percent: Option<u8>,
        error: Option<&str>,
    ) {
        let _ = crate::mobile::update_sync_notification(&serde_json::json!({
            "active": active,
            "phase": phase,
            "scanned": scanned,
            "total": total,
            "percent": percent,
            "error": error,
        }));
    }

    pub async fn sync_catalog_checked(
        &self,
        repo: &CatalogRepository,
        verify_deleted: bool,
    ) -> Result<usize, String> {
        let _gate = self.catalog_sync_gate.lock().await;
        self.reset_sync_cancel();
        let mut progress = crate::progress::SyncRun::new(&self.sync_progress);
        Self::publish_sync_notification(
            true,
            crate::progress::SyncPhase::Starting,
            0,
            None,
            Some(0),
            None,
        );
        let result = self
            .sync_catalog_inner(repo, &progress, verify_deleted)
            .await;
        let cancelled = result
            .as_ref()
            .is_err_and(|error| error == SYNC_CANCELLED_ERROR);
        if cancelled {
            progress.cancel();
        } else {
            progress.finish(result.as_ref().err().cloned());
        }
        match result.as_ref() {
            Ok(_) => Self::publish_sync_notification(
                false,
                crate::progress::SyncPhase::Complete,
                0,
                None,
                Some(100),
                None,
            ),
            Err(_) if cancelled => Self::publish_sync_notification(
                false,
                crate::progress::SyncPhase::Cancelled,
                0,
                None,
                None,
                None,
            ),
            Err(err) => Self::publish_sync_notification(
                false,
                crate::progress::SyncPhase::Error,
                0,
                None,
                None,
                Some(err),
            ),
        }
        self.reset_sync_cancel();
        result
    }

    async fn sync_catalog_inner(
        &self,
        repo: &CatalogRepository,
        progress: &crate::progress::SyncRun<'_>,
        verify_deleted: bool,
    ) -> Result<usize, String> {
        self.ensure_sync_not_cancelled()?;
        let chat = self.own_chat(repo).await?;
        self.ensure_sync_not_cancelled()?;
        let imported_snapshot =
            if repo.catalog_remote_count()? == 0 && repo.initial_catalog_sync_needed(chat)? {
                self.import_latest_catalog_snapshot(repo, chat)
                    .await?
                    .unwrap_or(0)
            } else {
                0
            };
        let checkpoint = repo.sync_checkpoint(chat)?;
        let document_checkpoint = repo.sync_stream_checkpoint(chat, "documents", checkpoint)?;
        let history_backfill_needed = !repo.history_backfill_done(chat)?;
        let fast_search_backfill_needed = !repo.fast_search_backfill_done(chat)?;
        let existing_remote_count = if fast_search_backfill_needed {
            repo.catalog_remote_count()?
        } else {
            0
        };
        let oldest_known_message = if history_backfill_needed || fast_search_backfill_needed {
            repo.catalog_oldest_message_id()?
        } else {
            None
        };
        let folder_checkpoint = repo.sync_stream_checkpoint(chat, "folders", checkpoint)?;
        let move_checkpoint = repo.sync_stream_checkpoint(chat, "moves", checkpoint)?;
        let trash_checkpoint = repo.sync_stream_checkpoint(chat, "trash", checkpoint)?;
        let delete_checkpoint = repo.sync_stream_checkpoint(chat, "deletes", checkpoint)?;

        // Folders, moves, trash and permanent-delete metadata are independent searches.
        // Give every stream its own checkpoint so delayed Telegram search indexing in
        // one stream can never cause another stream to skip a message permanently.
        progress.phase(crate::progress::SyncPhase::Folders);
        Self::publish_sync_notification(
            true,
            crate::progress::SyncPhase::Folders,
            0,
            None,
            Some(3),
            None,
        );
        let (
            (folders, folder_newest),
            (moves, move_newest),
            (trash_states, trash_newest),
            (deleted_message_ids, delete_newest),
        ) = tokio::try_join!(
            self.folder_events_since(chat, folder_checkpoint),
            self.file_move_events_since(chat, move_checkpoint),
            self.file_trash_events_since(chat, trash_checkpoint),
            self.file_delete_events_since(chat, delete_checkpoint),
        )?;
        let mut state = HistoryScanState {
            folders,
            moves,
            trash_states,
            deleted_message_ids,
            scanned: 0,
            total_documents: 0,
            newest: document_checkpoint.unwrap_or(0),
        };
        if !state.folders.is_empty() {
            repo.apply_folder_snapshot(state.folders.values().cloned().collect())?;
        }

        // Fast path for first/bootstrap repair: search only Nuvio captions and follow
        // TDLib's official next_from_message_id cursor. Older builds incorrectly used
        // the last returned message as the cursor and could stop at exactly 10k.
        // Keep history pagination as an automatic integrity fallback if TDLib reports
        // a suspiciously incomplete search.
        if fast_search_backfill_needed {
            let fast = self
                .scan_documents_fast(repo, progress, chat, &mut state)
                .await?;
            let hinted_total = fast.total_hint.max(0) as usize;
            let approximate_gap = hinted_total.saturating_sub(fast.fetched);
            let same_oldest_as_catalog =
                oldest_known_message.is_some() && fast.oldest_message_id == oldest_known_message;
            let suspicious_truncation = fast.stalled
                || !fast.exhausted
                || approximate_gap > 500
                || (existing_remote_count == 10_000
                    && fast.fetched <= 10_000
                    && same_oldest_as_catalog);

            // Search indexing can lag for very recent cross-device uploads. A short
            // history head scan up to the saved document checkpoint catches those
            // without turning normal sync into a full-history traversal.
            if document_checkpoint.is_some() {
                self.scan_history_range(repo, progress, chat, 0, document_checkpoint, &mut state)
                    .await?;
            }

            if suspicious_truncation {
                if let Some(oldest) = fast.oldest_message_id.or(oldest_known_message) {
                    self.scan_history_range(repo, progress, chat, oldest, None, &mut state)
                        .await?;
                } else {
                    self.scan_history_range(repo, progress, chat, 0, None, &mut state)
                        .await?;
                }
            }
        } else {
            // After the one-time fast backfill, correctness wins over search-index
            // freshness: incremental sync walks only the tiny history range newer than
            // the saved checkpoint and therefore catches unindexed messages too.
            self.scan_history_range(repo, progress, chat, 0, document_checkpoint, &mut state)
                .await?;
        }

        let mut missing: Vec<String> = state
            .deleted_message_ids
            .iter()
            .map(|id| format!("tg-{id}"))
            .collect();
        if verify_deleted && repo.should_verify_deleted(chat, 24 * 60 * 60)? {
            // Deep verification is now a periodic safety net for messages deleted
            // directly in Telegram. Nuvio-to-Nuvio deletes use #NuvioDelete1 and do
            // not require probing every known message on each manual sync.
            let known = repo.catalog_message_ids()?;
            for batch in known.chunks(100) {
                self.ensure_sync_not_cancelled()?;
                let e::Messages::Messages(page) = self
                    .sync_call(f::get_messages(chat, batch.to_vec(), self.client_id()))
                    .await?;
                let returned: Vec<_> = page
                    .messages
                    .iter()
                    .map(|msg| msg.as_ref().map(|m| m.id))
                    .collect();
                missing.extend(confirmed_missing(batch, &returned)?);
                for message in page.messages.into_iter().flatten() {
                    if let e::MessageContent::MessageDocument(content) = &message.content {
                        if let Some(mini) = content.document.minithumbnail.as_ref() {
                            let _ =
                                repo.cache_minithumbnail(&format!("tg-{}", message.id), &mini.data);
                        }
                    }
                }
            }
            repo.mark_deleted_verified(chat)?;
        }
        missing.sort_unstable();
        missing.dedup();

        progress.applying(0, state.total_documents.max(1));
        Self::publish_sync_notification(
            true,
            crate::progress::SyncPhase::Applying,
            state.scanned,
            None,
            Some(90),
            None,
        );
        repo.reconcile_moves_and_orphans(&state.moves)?;
        repo.reconcile_trash_states(&state.trash_states)?;
        repo.remove_catalog_files(&missing)
            .map_err(|e| e.to_string())?;

        // v1.2.2 and older stored Papelera only in the local SQLite catalog. On the
        // first sync after upgrading, publish only legacy `trashed=true` states so
        // an existing Windows/Android mismatch heals without a non-trashed device
        // overwriting a deletion made on the other device. Any newer remote restore
        // event has already been applied above and therefore suppresses this backfill.
        if !repo.trash_backfill_done(chat)? {
            let legacy_trashed = repo.trashed_remote_message_ids()?;
            for chunk in legacy_trashed.chunks(100) {
                self.ensure_sync_not_cancelled()?;
                let event = FileTrashEvent {
                    v: 1,
                    message_ids: chunk.to_vec(),
                    trashed: true,
                };
                self.send_sync_metadata_text(
                    chat,
                    format!(
                        "#NuvioTrash1 {}",
                        serde_json::to_string(&event).map_err(|e| e.to_string())?
                    ),
                )
                .await?;
            }
            repo.mark_trash_backfill_done(chat)?;
        }

        // Advance checkpoints only after every request and local reconciliation
        // succeeded. Metadata streams are independent from document search so a
        // delayed Telegram index in one stream cannot hide another stream's events.
        repo.save_sync_checkpoint(chat, state.newest)?;
        repo.save_sync_stream_checkpoint(chat, "documents", state.newest)?;
        repo.save_sync_stream_checkpoint(chat, "folders", folder_newest)?;
        repo.save_sync_stream_checkpoint(chat, "moves", move_newest)?;
        repo.save_sync_stream_checkpoint(chat, "trash", trash_newest)?;
        repo.save_sync_stream_checkpoint(chat, "deletes", delete_newest)?;
        if history_backfill_needed {
            repo.mark_history_backfill_done(chat)?;
        }
        if fast_search_backfill_needed {
            repo.mark_fast_search_backfill_done(chat)?;
        }
        repo.mark_initial_catalog_sync_done(chat)?;
        let snapshot_cursor = [
            state.newest,
            folder_newest,
            move_newest,
            trash_newest,
            delete_newest,
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        self.ensure_sync_not_cancelled()?;
        progress.publishing();
        Self::publish_sync_notification(
            true,
            crate::progress::SyncPhase::Publishing,
            state.scanned,
            None,
            Some(99),
            None,
        );
        self.publish_catalog_snapshot_if_needed(repo, chat, snapshot_cursor)
            .await?;
        progress.applying(state.total_documents, state.total_documents.max(1));
        Ok(imported_snapshot.saturating_add(state.total_documents))
    }

    pub async fn delete_files_permanently(
        &self,
        repo: &CatalogRepository,
        ids: &[String],
    ) -> Result<usize, String> {
        if ids.is_empty() {
            return Ok(0);
        }
        if ids.len() > 500 {
            return Err("Elimina como máximo 500 archivos por operación".into());
        }

        let files = repo.list_files().map_err(|e| e.to_string())?;
        let mut unique_ids = Vec::new();
        for id in ids {
            if unique_ids.iter().any(|existing: &String| existing == id) {
                continue;
            }
            let file = files
                .iter()
                .find(|file| file.id == *id)
                .ok_or_else(|| format!("El archivo {id} ya no existe en el catálogo"))?;
            if !file.trashed {
                return Err(format!(
                    "{} debe estar en la Papelera antes de eliminarse definitivamente",
                    file.name
                ));
            }
            unique_ids.push(id.clone());
        }

        let mut message_ids = Vec::with_capacity(unique_ids.len());
        for id in &unique_ids {
            message_ids.push(repo.remote(id)?.message_id);
        }
        let chat = self.own_chat(repo).await?;
        call(f::delete_messages(
            chat,
            message_ids.clone(),
            true,
            self.client_id(),
        ))
        .await?;

        // Persist the cross-device tombstone before forgetting the local row. If the
        // metadata send fails, keep the trashed local entry so the user can retry
        // instead of silently creating a Windows/Android catalog divergence.
        let event = FileDeleteEvent { v: 1, message_ids };
        self.send_metadata_text(
            chat,
            format!(
                "#NuvioDelete1 {}",
                serde_json::to_string(&event).map_err(|e| e.to_string())?
            ),
        )
        .await?;

        let removed = repo
            .remove_catalog_files(&unique_ids)
            .map_err(|e| e.to_string())?;
        Ok(removed)
    }

    pub async fn run_upload(&self, repo: &CatalogRepository, job: &WorkItem) -> Result<(), String> {
        let chat = self.own_chat(repo).await?;
        let mut pending = job.pending.unwrap_or(0);
        let mut file_updates = self.subscribe_file_updates();
        let mut upload_file_id: Option<i32> = None;

        if pending == 0 && job.pending.is_some() {
            self.sync_catalog(repo).await?;
            if repo
                .list_transfers()
                .map_err(|e| e.to_string())?
                .iter()
                .any(|t| t.id == job.id && t.status == "completed")
            {
                return Ok(());
            }
            repo.clear_pending(&job.id).map_err(|e| e.to_string())?;
            pending = 0;
        }

        if pending == 0 {
            // Fresh private staging was already hashed while it was prepared. Consume
            // its one-shot verification token after checking path/size/mtime; retries
            // and app restarts have no token and therefore re-hash cryptographically.
            let trusted_staging = repo.take_verified_staging(&job.id, &job.path, job.size);
            if !trusted_staging {
                let path = PathBuf::from(&job.path);
                let expected = job.sha256.clone();
                let expected_size = job.size;
                tauri::async_runtime::spawn_blocking(move || {
                    if fs::metadata(&path).map_err(|e| e.to_string())?.len() != expected_size as u64
                        || sha256_file(&path).map_err(|e| e.to_string())? != expected
                    {
                        return Err(
                            "El archivo cambió desde su selección. Selecciónalo de nuevo."
                                .to_string(),
                        );
                    }
                    Ok(())
                })
                .await
                .map_err(|e| e.to_string())??;
            }

            let text = format!(
                "#Nuvio1 {}",
                serde_json::to_string(&Caption {
                    v: 1,
                    transfer: job.id.clone(),
                    sha256: job.sha256.clone(),
                    folder_id: job.folder_id.clone(),
                })
                .map_err(|e| e.to_string())?
            );
            let content = e::InputMessageContent::InputMessageDocument(t::InputMessageDocument {
                document: e::InputFile::Local(t::InputFileLocal {
                    path: job.path.clone(),
                }),
                thumbnail: None,
                disable_content_type_detection: true,
                caption: Some(t::FormattedText {
                    text,
                    entities: vec![],
                }),
            });
            repo.set_pending(&job.id, 0).map_err(|e| e.to_string())?;
            let send_result = call(f::send_message(
                chat,
                None,
                None,
                None,
                content,
                self.client_id(),
            ))
            .await;
            let e::Message::Message(message) = match send_result {
                Ok(message) => message,
                Err(error) => {
                    if error.starts_with("Telegram ") && !error.contains("45 segundos") {
                        repo.clear_pending(&job.id).map_err(|e| e.to_string())?;
                    }
                    return Err(error);
                }
            };
            pending = message.id;
            repo.set_pending(&job.id, pending)
                .map_err(|e| e.to_string())?;
            if let e::MessageContent::MessageDocument(content) = &message.content {
                upload_file_id = Some(content.document.document.id);
                repo.set_td_file_id(&job.id, content.document.document.id)
                    .map_err(|e| e.to_string())?;
            }
            if let Some(doc) = document(&message) {
                repo.record_remote(&doc).map_err(|e| e.to_string())?;
                return Ok(());
            }
        }

        let deadline = Instant::now() + Duration::from_secs(3600);
        let mut estimator = SpeedEstimator::new(0);
        let mut last_uploaded = 0_i64;
        let mut last_progress_at = Instant::now();
        let mut waiting_reported = false;
        let mut last_message_probe = Instant::now() - Duration::from_secs(5);
        while Instant::now() < deadline {
            let control = repo.transfer_control(&job.id).map_err(|e| e.to_string())?;
            if control.pause_requested || control.cancel_requested {
                if pending > 0 {
                    let _ = call(f::delete_messages(
                        chat,
                        vec![pending],
                        true,
                        self.client_id(),
                    ))
                    .await;
                }
                repo.clear_pending(&job.id).map_err(|e| e.to_string())?;
                if control.cancel_requested {
                    repo.mark_cancelled(&job.id).map_err(|e| e.to_string())?;
                } else {
                    repo.mark_paused(&job.id).map_err(|e| e.to_string())?;
                }
                return Ok(());
            }

            let result = self
                .sent
                .lock()
                .map_err(|e| e.to_string())?
                .remove(&pending);
            if let Some(result) = result {
                match result {
                    Ok(message) => {
                        let doc = document(&message)
                            .ok_or("Telegram confirmó un contenido inesperado")?;
                        repo.record_remote(&doc).map_err(|e| e.to_string())?;
                        return Ok(());
                    }
                    Err(error) => {
                        repo.clear_pending(&job.id).map_err(|e| e.to_string())?;
                        return Err(error);
                    }
                }
            }

            if upload_file_id.is_none() || last_message_probe.elapsed() >= Duration::from_secs(5) {
                last_message_probe = Instant::now();
                match call(f::get_message(chat, pending, self.client_id())).await {
                    Ok(e::Message::Message(message)) => {
                        if let Some(doc) = document(&message) {
                            repo.record_remote(&doc).map_err(|e| e.to_string())?;
                            return Ok(());
                        }
                        if let Some(e::MessageSendingState::Failed(_)) = message.sending_state {
                            repo.clear_pending(&job.id).map_err(|e| e.to_string())?;
                            return Err("Telegram rechazó la subida. Revisa la conexión o el tamaño y reintenta.".into());
                        }
                        if let e::MessageContent::MessageDocument(content) = &message.content {
                            upload_file_id = Some(content.document.document.id);
                            repo.set_td_file_id(&job.id, content.document.document.id)
                                .map_err(|e| e.to_string())?;
                        }
                    }
                    Err(_) if upload_file_id.is_none() => {
                        self.sync_catalog(repo).await?;
                        if repo
                            .transfer_by_id(&job.id)
                            .map_err(|e| e.to_string())?
                            .is_some_and(|transfer| transfer.status == "completed")
                        {
                            return Ok(());
                        }
                        return Err("No se pudo confirmar la subida anterior. Nuvio la reintentará sin perder el resto de la cola.".into());
                    }
                    Err(_) => {}
                }
            }

            if let Some(file_id) = upload_file_id {
                if let Ok(file) = self
                    .next_file_update(&mut file_updates, file_id, Duration::from_secs(1))
                    .await
                {
                    let uploaded = file.remote.uploaded_size.clamp(0, job.size.max(0));
                    let phase = if uploaded >= job.size && job.size > 0 {
                        "confirming"
                    } else {
                        "uploading"
                    };
                    if uploaded > last_uploaded || phase == "confirming" {
                        let speed = estimator.update(uploaded);
                        let eta = SpeedEstimator::eta(job.size, uploaded, speed);
                        last_uploaded = uploaded;
                        last_progress_at = Instant::now();
                        waiting_reported = false;
                        repo.update_runtime(
                            &job.id,
                            phase,
                            phase,
                            uploaded,
                            job.size,
                            speed,
                            eta,
                            if phase == "confirming" {
                                "Confirmando en Telegram"
                            } else {
                                "Subiendo a Telegram"
                            },
                            None,
                        )
                        .map_err(|e| e.to_string())?;
                    } else if !waiting_reported
                        && last_progress_at.elapsed() >= Duration::from_secs(8)
                    {
                        // Do not replace a valid ETA with "calculando" on every duplicate
                        // TDLib sample. Only mark a real sustained pause after eight seconds.
                        repo.update_runtime(
                            &job.id,
                            "uploading",
                            "uploading",
                            uploaded,
                            job.size,
                            0,
                            None,
                            "Esperando progreso de Telegram",
                            None,
                        )
                        .map_err(|e| e.to_string())?;
                        waiting_reported = true;
                    }
                }
            } else {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        Err("La subida excedió el tiempo máximo de confirmación.".into())
    }

    pub async fn run_download(
        &self,
        repo: &CatalogRepository,
        job: &WorkItem,
    ) -> Result<(), String> {
        let chat = self.own_chat(repo).await?;
        let message_id = job.pending.ok_or("Falta la referencia de Telegram")?;
        let e::Message::Message(message) =
            call(f::get_message(chat, message_id, self.client_id())).await?;
        let e::MessageContent::MessageDocument(content) = message.content else {
            return Err("El mensaje ya no contiene el archivo".into());
        };
        let file_id = content.document.document.id;
        repo.set_td_file_id(&job.id, file_id)
            .map_err(|e| e.to_string())?;
        let mut file_updates = self.subscribe_file_updates();
        let e::File::File(mut file) =
            call(f::download_file(file_id, 16, 0, 0, false, self.client_id())).await?;
        let deadline = Instant::now() + Duration::from_secs(3600);
        let mut estimator = SpeedEstimator::new(0);

        while Instant::now() < deadline {
            let control = repo.transfer_control(&job.id).map_err(|e| e.to_string())?;
            if control.pause_requested || control.cancel_requested {
                let _ = call(f::cancel_download_file(file_id, false, self.client_id())).await;
                if control.cancel_requested {
                    repo.mark_cancelled(&job.id).map_err(|e| e.to_string())?;
                } else {
                    repo.mark_paused(&job.id).map_err(|e| e.to_string())?;
                }
                return Ok(());
            }

            if file.local.is_downloading_completed {
                repo.update_runtime(
                    &job.id,
                    "confirming",
                    "confirming",
                    job.size,
                    job.size,
                    0,
                    Some(0),
                    "Verificando SHA-256",
                    None,
                )
                .map_err(|e| e.to_string())?;
                let source = PathBuf::from(file.local.path);
                let target = PathBuf::from(&job.path);
                let hash = job.sha256.clone();
                let size = job.size;
                tauri::async_runtime::spawn_blocking(move || {
                    verified_copy(&source, &target, &hash, size)
                })
                .await
                .map_err(|e| e.to_string())??;
                repo.update_runtime(
                    &job.id,
                    "completed",
                    "completed",
                    job.size,
                    job.size,
                    0,
                    Some(0),
                    "Descarga verificada",
                    None,
                )
                .map_err(|e| e.to_string())?;
                return Ok(());
            }
            if !file.local.is_downloading_active {
                return Err("La descarga se interrumpió. Nuvio intentará reanudarla.".into());
            }
            let downloaded = file.local.downloaded_size.clamp(0, job.size.max(0));
            let speed = estimator.update(downloaded);
            let eta = SpeedEstimator::eta(job.size, downloaded, speed);
            repo.update_runtime(
                &job.id,
                "downloading",
                "downloading",
                downloaded,
                job.size,
                speed,
                eta,
                "Descargando",
                None,
            )
            .map_err(|e| e.to_string())?;
            file = self
                .next_file_update(&mut file_updates, file_id, Duration::from_secs(1))
                .await?;
        }
        Err("La descarga excedió el tiempo máximo permitido.".into())
    }
}

pub fn verified_copy(
    source: &Path,
    target: &Path,
    expected_hash: &str,
    size: i64,
) -> Result<(), String> {
    #[cfg(target_os = "android")]
    if let Some(data) = target.to_str().and_then(|p| p.strip_prefix("nuvio-saf:")) {
        let destination: AndroidDestination =
            serde_json::from_str(data).map_err(|e| e.to_string())?;
        validate_download_name(&destination.name)?;
        return crate::mobile::publish_download(
            source,
            &destination.tree,
            &destination.name,
            &destination.policy,
            expected_hash,
            size,
        );
    }
    use std::io::{Read, Write};
    if target.exists() {
        return Err("El destino ya existe; Nuvio no sobrescribe archivos.".into());
    }
    let parent = target.parent().ok_or("Destino inválido")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    let mut input = fs::File::open(source).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        tmp.write_all(&buffer[..read]).map_err(|e| e.to_string())?;
        hasher.update(&buffer[..read]);
        copied += read as u64;
    }
    tmp.flush().map_err(|e| e.to_string())?;
    let actual_hash = hex::encode(hasher.finalize());
    if copied != size as u64 || actual_hash != expected_hash {
        return Err(
            "La descarga no coincide con el original. No se guardó una copia dañada.".into(),
        );
    }
    tmp.as_file().sync_all().map_err(|e| e.to_string())?;
    tmp.persist_noclobber(target).map_err(|e| e.to_string())?;
    Ok(())
}

fn confirmed_missing(expected: &[i64], returned: &[Option<i64>]) -> Result<Vec<String>, String> {
    if expected.len() != returned.len() {
        return Err(
            "Telegram devolvió una comprobación incompleta; se conservaron los archivos.".into(),
        );
    }
    let mut missing = Vec::new();
    for (id, actual) in expected.iter().zip(returned) {
        match actual {
            None => missing.push(format!("tg-{id}")),
            Some(actual) if actual == id => {}
            Some(_) => {
                return Err(
                    "Telegram devolvió mensajes inesperados; se conservaron los archivos.".into(),
                )
            }
        }
    }
    Ok(missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_bootstrap_runs_only_for_a_fresh_catalog() {
        let fresh = tempfile::tempdir().unwrap();
        let fresh_repo = CatalogRepository::open(&fresh.path().join("db")).unwrap();
        fresh_repo.init_cloud().unwrap();
        assert!(fresh_repo.catalog_bootstrap_required_locally().unwrap());

        fresh_repo.save_sync_checkpoint(42, 100).unwrap();
        assert!(!fresh_repo.catalog_bootstrap_required_locally().unwrap());

        let marked = tempfile::tempdir().unwrap();
        let marked_repo = CatalogRepository::open(&marked.path().join("db")).unwrap();
        marked_repo.init_cloud().unwrap();
        marked_repo.mark_initial_catalog_sync_done(77).unwrap();
        assert!(!marked_repo.catalog_bootstrap_required_locally().unwrap());
    }

    #[test]
    fn saved_messages_chat_binding_survives_restart() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        {
            let repo = CatalogRepository::open(&path).unwrap();
            repo.init_cloud().unwrap();
            assert_eq!(repo.bound_chat().unwrap(), None);
            repo.bind_chat(987_654_321).unwrap();
            assert_eq!(repo.bound_chat().unwrap(), Some(987_654_321));
        }
        let repo = CatalogRepository::open(&path).unwrap();
        repo.init_cloud().unwrap();
        assert_eq!(repo.bound_chat().unwrap(), Some(987_654_321));
    }

    #[test]
    fn sync_checkpoint_survives_restart_and_is_scoped_to_chat() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        {
            let repo = CatalogRepository::open(&path).unwrap();
            repo.init_cloud().unwrap();
            assert_eq!(repo.sync_checkpoint(1).unwrap(), None);
            repo.save_sync_checkpoint(1, 50).unwrap();
            assert_eq!(
                repo.sync_stream_checkpoint(1, "folders", Some(50)).unwrap(),
                Some(50)
            );
            repo.save_sync_stream_checkpoint(1, "folders", 44).unwrap();
            repo.record_remote(&test_document(100, "local-upload.txt"))
                .unwrap();
            assert_eq!(repo.sync_checkpoint(1).unwrap(), Some(50));
            assert_eq!(
                repo.sync_stream_checkpoint(1, "folders", Some(50)).unwrap(),
                Some(44)
            );
        }
        let repo = CatalogRepository::open(&path).unwrap();
        assert_eq!(repo.sync_checkpoint(1).unwrap(), Some(50));
        assert_eq!(repo.sync_checkpoint(2).unwrap(), None);
        assert_eq!(
            repo.sync_stream_checkpoint(1, "folders", Some(50)).unwrap(),
            Some(44)
        );
        assert_eq!(
            repo.sync_stream_checkpoint(1, "moves", Some(50)).unwrap(),
            Some(50)
        );
        assert_eq!(
            repo.sync_stream_checkpoint(2, "folders", None).unwrap(),
            None
        );
    }

    #[test]
    fn history_backfill_marker_and_oldest_message_are_chat_scoped() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        assert!(!repo.history_backfill_done(1).unwrap());
        assert_eq!(repo.catalog_oldest_message_id().unwrap(), None);

        repo.record_remote(&test_document(20_000, "newer.txt"))
            .unwrap();
        repo.record_remote(&test_document(10_001, "oldest-known.txt"))
            .unwrap();
        assert_eq!(repo.catalog_oldest_message_id().unwrap(), Some(10_001));

        repo.mark_history_backfill_done(1).unwrap();
        assert!(repo.history_backfill_done(1).unwrap());
        assert!(!repo.history_backfill_done(2).unwrap());
    }

    #[test]
    fn catalog_snapshot_v2_round_trip_rebuilds_resolved_catalog_and_checkpoints() {
        let source_root = tempfile::tempdir().unwrap();
        let source = CatalogRepository::open(&source_root.path().join("source.db")).unwrap();
        source.init_cloud().unwrap();
        source
            .apply_folder_event("folder-root", "Documentos", None, false)
            .unwrap();
        source
            .apply_folder_event("folder-child", "Proyectos", Some("folder-root"), false)
            .unwrap();

        let mut first = test_document(101, "one.txt");
        first.folder_id = Some("folder-child".into());
        first.minithumbnail = Some("ignored-preview".into());
        let second = test_document(102, "two.bin");
        source.record_remote_batch(&[first, second]).unwrap();
        source
            .reconcile_trash_states(&std::collections::HashMap::from([(102, true)]))
            .unwrap();

        source.save_sync_checkpoint(42, 220).unwrap();
        source
            .save_sync_stream_checkpoint(42, "documents", 220)
            .unwrap();
        source
            .save_sync_stream_checkpoint(42, "folders", 210)
            .unwrap();
        source
            .save_sync_stream_checkpoint(42, "moves", 205)
            .unwrap();
        source
            .save_sync_stream_checkpoint(42, "trash", 215)
            .unwrap();
        source
            .save_sync_stream_checkpoint(42, "deletes", 200)
            .unwrap();

        let snapshot = source.export_catalog_snapshot(42).unwrap();
        assert_eq!(snapshot.v, 2);
        assert_eq!(snapshot.documents.len(), 2);
        assert!(snapshot
            .documents
            .iter()
            .all(|document| document.minithumbnail.is_none()));
        assert_eq!(
            snapshot.checkpoints,
            CatalogSnapshotCheckpoints {
                documents: 220,
                folders: 210,
                moves: 205,
                trash: 215,
                deletes: 200,
            }
        );
        assert_eq!(snapshot.trashed_message_ids, vec![102]);

        let zip_path = source_root.path().join("catalog-v2.zip");
        write_catalog_snapshot_zip(&snapshot, &zip_path).unwrap();
        let decoded = read_catalog_snapshot_zip(&zip_path).unwrap();
        assert_eq!(decoded.documents.len(), 2);
        assert_eq!(decoded.folders.len(), 2);

        let target_root = tempfile::tempdir().unwrap();
        let target = CatalogRepository::open(&target_root.path().join("target.db")).unwrap();
        target.init_cloud().unwrap();
        assert_eq!(target.import_catalog_snapshot(42, &decoded).unwrap(), 2);

        let files = target.list_files().unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|file| {
            file.id == "tg-101"
                && file.folder_id.as_deref() == Some("folder-child")
                && !file.trashed
        }));
        assert!(files.iter().any(|file| file.id == "tg-102" && file.trashed));

        let folders = target.list_folders().unwrap();
        assert!(folders.iter().any(|folder| {
            folder.id == "folder-child" && folder.parent_id.as_deref() == Some("folder-root")
        }));
        assert_eq!(target.sync_checkpoint(42).unwrap(), Some(220));
        assert_eq!(
            target.sync_stream_checkpoint(42, "folders", None).unwrap(),
            Some(210)
        );
        assert!(target.history_backfill_done(42).unwrap());
        assert!(target.fast_search_backfill_done(42).unwrap());
        assert!(!target.initial_catalog_sync_needed(42).unwrap());

        let caption = CatalogSnapshotCaption {
            v: 2,
            sha256: "b".repeat(64),
            documents: 2,
            generated_at: decoded.generated_at,
        };
        let text = format!(
            "#NuvioCatalog2 {}",
            serde_json::to_string(&caption).unwrap()
        );
        let parsed = parse_catalog_snapshot_caption(&text).unwrap();
        assert_eq!(parsed.documents, 2);
        assert_eq!(parsed.sha256, "b".repeat(64));
    }

    #[test]
    fn catalog_has_no_ten_thousand_file_ceiling() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        const TOTAL: i64 = 30_123;
        for first in (1..=TOTAL).step_by(100) {
            let last = (first + 99).min(TOTAL);
            let mut batch = Vec::with_capacity((last - first + 1) as usize);
            for message_id in first..=last {
                let mut doc = test_document(message_id, &format!("file-{message_id}.bin"));
                doc.transfer.clear();
                batch.push(doc);
            }
            repo.record_remote_batch(&batch).unwrap();
        }
        assert_eq!(repo.list_files().unwrap().len(), TOTAL as usize);
        assert_eq!(repo.catalog_message_ids().unwrap().len(), TOTAL as usize);
        assert_eq!(repo.sync_file_delta(0).unwrap().files.len(), TOTAL as usize);
        assert_eq!(repo.catalog_oldest_message_id().unwrap(), Some(1));
    }

    #[test]
    fn deletion_verification_rejects_partial_or_unrelated_results() {
        assert!(confirmed_missing(&[1, 2], &[None]).is_err());
        assert!(confirmed_missing(&[1, 2], &[None, Some(3)]).is_err());
        assert_eq!(
            confirmed_missing(&[1, 2], &[None, Some(2)]).unwrap(),
            vec!["tg-1"]
        );
    }

    #[test]
    fn permanent_delete_metadata_is_compact_and_parseable() {
        let encoded = format!(
            "#NuvioDelete1 {}",
            serde_json::to_string(&FileDeleteEvent {
                v: 1,
                message_ids: vec![41, 42, 43],
            })
            .unwrap()
        );
        let parsed = parse_file_delete_text(&encoded).expect("valid delete event");
        assert_eq!(parsed.message_ids, vec![41, 42, 43]);
        assert!(parse_file_delete_text("#NuvioDelete1 {\"v\":2,\"message_ids\":[1]}").is_none());
    }

    #[test]
    fn deep_delete_verification_is_periodic_instead_of_every_sync() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        assert!(!repo.should_verify_deleted(7, 86_400).unwrap());
        repo.connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE app_meta SET value='0' WHERE key='catalog_delete_verify_v2:7'",
                [],
            )
            .unwrap();
        assert!(repo.should_verify_deleted(7, 86_400).unwrap());
        repo.mark_deleted_verified(7).unwrap();
        assert!(!repo.should_verify_deleted(7, 86_400).unwrap());
    }

    #[test]
    fn trash_metadata_reconciles_between_device_catalogs() {
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let repo_a = CatalogRepository::open(&root_a.path().join("db")).unwrap();
        let repo_b = CatalogRepository::open(&root_b.path().join("db")).unwrap();
        repo_a.init_cloud().unwrap();
        repo_b.init_cloud().unwrap();
        let doc = test_document(42, "shared.txt");
        repo_a.record_remote(&doc).unwrap();
        repo_b.record_remote(&doc).unwrap();

        let encoded = format!(
            "#NuvioTrash1 {}",
            serde_json::to_string(&FileTrashEvent {
                v: 1,
                message_ids: vec![42],
                trashed: true,
            })
            .unwrap()
        );
        let event = parse_file_trash_text(&encoded).expect("valid trash event");
        let states = event
            .message_ids
            .into_iter()
            .map(|id| (id, event.trashed))
            .collect();
        repo_b.reconcile_trash_states(&states).unwrap();
        assert!(repo_b.list_files().unwrap()[0].trashed);
        assert!(!repo_a.list_files().unwrap()[0].trashed);

        let mut restored = std::collections::HashMap::new();
        restored.insert(42, false);
        repo_b.reconcile_trash_states(&restored).unwrap();
        assert!(!repo_b.list_files().unwrap()[0].trashed);
    }

    #[test]
    fn legacy_trash_backfill_is_one_time_and_remote_restore_wins_first() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        repo.record_remote(&test_document(42, "legacy-trash.txt"))
            .unwrap();
        repo.record_remote(&test_document(43, "visible.txt"))
            .unwrap();
        repo.trash_many(&["tg-42".into()], true).unwrap();

        assert!(!repo.trash_backfill_done(99).unwrap());
        assert_eq!(repo.trashed_remote_message_ids().unwrap(), vec![42]);

        let mut restored = std::collections::HashMap::new();
        restored.insert(42, false);
        repo.reconcile_trash_states(&restored).unwrap();
        assert!(repo.trashed_remote_message_ids().unwrap().is_empty());

        repo.mark_trash_backfill_done(99).unwrap();
        assert!(repo.trash_backfill_done(99).unwrap());
        assert!(!repo.trash_backfill_done(100).unwrap());
    }

    #[test]
    fn remote_minithumbnail_cache_is_backward_compatible() {
        let legacy = serde_json::json!({
            "transfer": "transfer-9",
            "message_id": 9,
            "name": "photo.jpg",
            "size": 3,
            "date": 1,
            "sha256": "a".repeat(64),
            "folder_id": null
        });
        let decoded: RemoteDocument = serde_json::from_value(legacy).unwrap();
        assert!(decoded.minithumbnail.is_none());

        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        repo.record_remote(&decoded).unwrap();
        repo.cache_minithumbnail("tg-9", "tiny-base64-preview")
            .unwrap();
        assert_eq!(
            repo.remote("tg-9").unwrap().minithumbnail.as_deref(),
            Some("tiny-base64-preview")
        );
    }

    #[test]
    fn folders_can_be_committed_before_progressive_file_publication() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        repo.apply_folder_snapshot(vec![FolderEvent {
            v: 1,
            id: "folder-first".into(),
            name: "Fotos".into(),
            parent_id: None,
            trashed: false,
        }])
        .unwrap();

        assert!(repo.list_files().unwrap().is_empty());
        assert_eq!(repo.list_folders().unwrap().len(), 1);

        let mut doc = test_document(601, "photo.jpg");
        doc.folder_id = Some("folder-first".into());
        repo.record_remote_batch(&[doc]).unwrap();
        let files = repo.list_files().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].folder_id.as_deref(), Some("folder-first"));
        assert_eq!(files[0].folder, "Fotos");
    }

    #[test]
    fn progressive_catalog_pages_are_visible_before_final_reconciliation() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();

        let mut first_page = test_document(501, "newest.txt");
        first_page.folder_id = Some("folder-later".into());
        repo.record_remote_batch(&[first_page.clone()]).unwrap();

        let live = repo.list_files().unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, "tg-501");
        assert!(
            live[0].folder_id.is_none(),
            "unknown folders fall back safely to root during the live scan"
        );

        repo.record_remote_batch(&[test_document(500, "older.txt")])
            .unwrap();
        assert_eq!(
            repo.list_files().unwrap().len(),
            2,
            "a second page is visible before sync completion"
        );

        repo.apply_folder_snapshot(vec![FolderEvent {
            v: 1,
            id: "folder-later".into(),
            name: "Sincronizada".into(),
            parent_id: None,
            trashed: false,
        }])
        .unwrap();
        repo.record_remote_batch(&[first_page]).unwrap();

        let reconciled = repo
            .list_files()
            .unwrap()
            .into_iter()
            .find(|file| file.id == "tg-501")
            .unwrap();
        assert_eq!(reconciled.folder_id.as_deref(), Some("folder-later"));
        assert_eq!(reconciled.folder, "Sincronizada");
    }

    #[test]
    fn deletion_verification_uses_visible_catalog_ids_even_without_remote_cache_row() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        repo.record_remote(&test_document(77, "deleted-elsewhere.txt"))
            .unwrap();
        repo.connection
            .lock()
            .unwrap()
            .execute("DELETE FROM remote_documents WHERE id='tg-77'", [])
            .unwrap();

        assert_eq!(repo.catalog_message_ids().unwrap(), vec![77]);
        let missing = confirmed_missing(&[77], &[None]).unwrap();
        assert_eq!(repo.remove_catalog_files(&missing).unwrap(), 1);
        assert!(repo.list_files().unwrap().is_empty());
    }

    fn test_document(message_id: i64, name: &str) -> RemoteDocument {
        RemoteDocument {
            transfer: format!("transfer-{message_id}"),
            message_id,
            name: name.into(),
            size: 3,
            date: 1,
            sha256: "a".repeat(64),
            folder_id: None,
            minithumbnail: None,
        }
    }

    #[test]
    fn queued_downloads_reserve_names_across_batches() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        repo.record_remote(&test_document(1, "same.txt")).unwrap();
        repo.record_remote(&test_document(2, "same.txt")).unwrap();
        let batch = repo.enqueue_downloads(&["tg-1".into(), "tg-2".into()], root.path(), "rename");
        assert_eq!(batch[0].status, "queued");
        assert_eq!(batch[1].status, "queued");
        assert_ne!(batch[0].path, batch[1].path);
        let skipped = repo.enqueue_downloads(&["tg-1".into()], root.path(), "skip");
        assert_eq!(skipped[0].status, "skipped");
    }

    #[test]
    fn downloads_respect_paths_reserved_by_older_catalogs() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        repo.record_remote(&test_document(1, "legacy.txt")).unwrap();
        repo.enqueue_downloads(&["tg-1".into()], root.path(), "skip");
        repo.connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE transfer_metadata SET local_path=?1",
                [root
                    .path()
                    .join("legacy.txt")
                    .to_string_lossy()
                    .into_owned()],
            )
            .unwrap();
        let results = repo.enqueue_downloads(&["tg-1".into()], root.path(), "skip");
        assert_eq!(results[0].status, "skipped");
    }

    #[test]
    fn download_rejects_remote_paths_outside_selected_directory() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        for name in [
            "../escape.txt",
            "..\\escape.txt",
            "C:\\escape.txt",
            "/escape.txt",
            "nested/file.txt",
            "file:stream",
            "",
        ] {
            repo.record_remote(&test_document(1, name)).unwrap();
            let result = repo.enqueue_downloads(&["tg-1".into()], root.path(), "rename");
            assert_eq!(result[0].status, "error", "accepted unsafe name: {name}");
        }
        assert!(repo.list_transfers().unwrap().is_empty());
    }

    #[test]
    fn sync_updates_file_kind_and_timestamp_without_losing_local_flags() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        let mut doc = test_document(1, "before.txt");
        repo.record_remote(&doc).unwrap();
        repo.set_favorite("tg-1", true).unwrap();
        repo.trash_many(&["tg-1".into()], true).unwrap();
        doc.name = "after.mp3".into();
        doc.date = 2;
        repo.record_remote(&doc).unwrap();
        let file = repo.list_files().unwrap().remove(0);
        assert_eq!(file.kind, "audio");
        assert_eq!(file.updated_at, "1970-01-01T00:00:02Z");
        assert!(file.favorite && file.trashed);
    }

    #[test]
    fn android_downloads_reserve_names_across_batches_and_preserve_tree_uri() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        repo.record_remote(&test_document(1, "vacación.txt"))
            .unwrap();
        let tree = "content://com.android.externalstorage.documents/tree/primary%3AMis%20archivos";
        let first = repo.enqueue_android_downloads(
            &["tg-1".into()],
            tree,
            "rename",
            vec!["vacación.txt".into()],
        );
        assert_eq!(first[0].path.as_deref(), Some("vacación (1).txt"));
        let second = repo.enqueue_android_downloads(
            &["tg-1".into()],
            tree,
            "rename",
            vec!["vacación.txt".into()],
        );
        assert_eq!(second[0].path.as_deref(), Some("vacación (2).txt"));
        let job = repo.claim_pending("download").unwrap().unwrap();
        let destination: AndroidDestination =
            serde_json::from_str(job.path.strip_prefix("nuvio-saf:").unwrap()).unwrap();
        assert_eq!(destination.tree, tree);
        assert_eq!(destination.name, "vacación (1).txt");
        let skipped = repo.enqueue_android_downloads(
            &["tg-1".into()],
            tree,
            "skip",
            vec!["vacación.txt".into()],
        );
        assert_eq!(skipped[0].status, "skipped");
        assert_eq!(repo.list_transfers().unwrap().len(), 2);
    }

    #[test]
    fn android_rejects_unsafe_names_and_invalid_destinations() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        repo.record_remote(&test_document(1, "../outside.txt"))
            .unwrap();
        let result = repo.enqueue_android_downloads(
            &["tg-1".into()],
            "content://provider/tree/folder",
            "rename",
            vec![],
        );
        assert_eq!(result[0].status, "error");
        repo.record_remote(&test_document(1, "safe.txt")).unwrap();
        let result =
            repo.enqueue_android_downloads(&["tg-1".into()], "file:///tmp", "rename", vec![]);
        assert_eq!(result[0].status, "error");
        assert!(repo.list_transfers().unwrap().is_empty());
    }

    #[test]
    fn folder_sync_rebuilds_changed_hierarchy_and_breaks_remote_cycles() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        repo.create_folder_local("folder-a", "A", None).unwrap();
        repo.create_folder_local("folder-b", "B", Some("folder-a"))
            .unwrap();
        let event = |id: &str, parent: Option<&str>| FolderEvent {
            v: 1,
            id: id.into(),
            name: id.into(),
            parent_id: parent.map(str::to_owned),
            trashed: false,
        };
        repo.apply_folder_snapshot(vec![
            event("folder-a", Some("folder-b")),
            event("folder-b", None),
        ])
        .unwrap();
        assert_eq!(
            repo.folder_by_id("folder-a").unwrap().parent_id.as_deref(),
            Some("folder-b")
        );
        assert!(repo.folder_by_id("folder-b").unwrap().parent_id.is_none());
        repo.apply_folder_snapshot(vec![
            event("folder-a", Some("folder-b")),
            event("folder-b", Some("folder-a")),
        ])
        .unwrap();
        assert!(repo.folder_by_id("folder-b").unwrap().parent_id.is_none());
        let mut doc = test_document(1, "safe.txt");
        doc.folder_id = Some("folder-a".into());
        repo.record_remote(&doc).unwrap();
        assert_eq!(repo.list_files().unwrap()[0].folder, "folder-b / folder-a");
    }

    #[test]
    fn absent_or_deleted_remote_folder_does_not_break_catalog_sync() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        let mut doc = test_document(1, "safe.txt");
        doc.folder_id = Some("folder-deleted".into());
        repo.record_remote(&doc).unwrap();
        assert!(repo.list_files().unwrap()[0].folder_id.is_none());
        repo.apply_folder_event("folder-deleted", "Deleted", None, true)
            .unwrap();
        repo.record_remote(&doc).unwrap();
        assert!(repo.list_files().unwrap()[0].folder_id.is_none());
    }

    #[test]
    fn real_catalog_starts_empty() {
        let temp = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&temp.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        assert!(repo.list_files().unwrap().is_empty());
        assert!(repo.list_transfers().unwrap().is_empty());
        repo.bind_account(1).unwrap();
        assert!(repo.bind_account(2).is_err());
    }

    #[test]
    fn confirmed_upload_is_atomic_and_preserves_favorites() {
        let temp = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&temp.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        repo.enqueue_transfer(
            "transfer-1",
            "a.txt",
            "a.txt",
            3,
            &"a".repeat(64),
            false,
            "uploading",
        )
        .unwrap();
        let doc = RemoteDocument {
            transfer: "transfer-1".into(),
            message_id: 123456789123456,
            name: "a.txt".into(),
            size: 3,
            date: 1,
            sha256: "a".repeat(64),
            folder_id: None,
            minithumbnail: None,
        };
        repo.record_remote(&doc).unwrap();
        let file = repo.list_files().unwrap().remove(0);
        repo.set_favorite(&file.id, true).unwrap();
        repo.record_remote(&doc).unwrap();
        assert_eq!(repo.list_files().unwrap().len(), 1);
        assert!(repo.list_files().unwrap()[0].favorite);
        assert_eq!(repo.list_transfers().unwrap()[0].status, "completed");
    }

    #[test]
    fn download_rejects_corruption_and_never_overwrites() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("src");
        let target = temp.path().join("target");
        fs::write(&source, b"abc").unwrap();
        let hash = sha256_file(&source).unwrap();
        assert!(verified_copy(&source, &target, "wrong", 3).is_err());
        assert!(!target.exists());
        verified_copy(&source, &target, &hash, 3).unwrap();
        assert!(verified_copy(&source, &target, &hash, 3).is_err());
    }

    #[test]
    fn batch_download_can_skip_or_rename_existing_files() {
        let temp = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&temp.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        let doc = RemoteDocument {
            transfer: "transfer-1".into(),
            message_id: 12,
            name: "a.txt".into(),
            size: 3,
            date: 1,
            sha256: "a".repeat(64),
            folder_id: None,
            minithumbnail: None,
        };
        repo.record_remote(&doc).unwrap();
        fs::write(temp.path().join("a.txt"), b"old").unwrap();
        let skip = repo.enqueue_downloads(&["tg-12".into()], temp.path(), "skip");
        assert_eq!(skip[0].status, "skipped");
        let rename = repo.enqueue_downloads(&["tg-12".into()], temp.path(), "rename");
        assert_eq!(rename[0].status, "queued");
        assert!(rename[0].path.as_ref().unwrap().contains("(1)"));
    }

    #[test]
    fn common_extensions_are_classified_into_specific_kinds() {
        let cases = [
            ("jpg", "image"),
            ("m2ts", "video"),
            ("opus", "audio"),
            ("pdf", "pdf"),
            ("docx", "document"),
            ("xlsx", "spreadsheet"),
            ("pptx", "presentation"),
            ("md", "text"),
            ("rs", "code"),
            ("7z", "archive"),
            ("epub", "ebook"),
            ("sqlite", "database"),
            ("woff2", "font"),
            ("apk", "package"),
            ("stl", "model"),
            ("unknown-extension", "other"),
        ];
        for (extension, expected) in cases {
            assert_eq!(
                classify_extension(extension),
                expected,
                "extension {extension}"
            );
        }
    }

    #[test]
    fn trash_supports_bulk_restore_and_catalog_purge() {
        let temp = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&temp.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        repo.record_remote(&test_document(21, "one.pdf")).unwrap();
        repo.record_remote(&test_document(22, "two.xlsx")).unwrap();
        let ids = vec!["tg-21".to_string(), "tg-22".to_string()];

        assert_eq!(repo.trash_many(&ids, true).unwrap(), 2);
        assert_eq!(repo.trashed_ids().unwrap().len(), 2);
        assert_eq!(repo.trash_many(&["tg-21".into()], false).unwrap(), 1);
        assert_eq!(repo.trashed_ids().unwrap(), vec!["tg-22".to_string()]);

        assert_eq!(repo.remove_catalog_files(&["tg-22".into()]).unwrap(), 1);
        assert!(repo.remote("tg-22").is_err());
        assert!(repo
            .list_files()
            .unwrap()
            .iter()
            .all(|file| file.id != "tg-22"));
    }
    #[test]
    fn catalog_batch_rolls_back_on_error() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        repo.connection.lock().unwrap().execute_batch("CREATE TRIGGER fail_test BEFORE INSERT ON files WHEN NEW.id='tg-2' BEGIN SELECT RAISE(ABORT, 'test failure'); END;").unwrap();
        assert!(repo
            .record_remote_batch(&[test_document(1, "a.txt"), test_document(2, "b.txt")])
            .is_err());
        assert!(repo.list_files().unwrap().is_empty());
    }
    #[test]
    fn catalog_batch_keeps_favorites_and_indexes_all_documents() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        repo.record_remote(&test_document(1, "a.txt")).unwrap();
        repo.set_favorite("tg-1", true).unwrap();
        let documents: Vec<_> = (1..=500)
            .map(|id| test_document(id, &format!("{id}.txt")))
            .collect();
        let start = Instant::now();
        for batch in documents.chunks(250) {
            repo.record_remote_batch(batch).unwrap();
        }
        println!("500 documents in batch: {:?}", start.elapsed());
        let files = repo.list_files().unwrap();
        assert_eq!(files.len(), 500);
        assert!(
            files
                .iter()
                .find(|file| file.id == "tg-1")
                .unwrap()
                .favorite
        );
    }
}
