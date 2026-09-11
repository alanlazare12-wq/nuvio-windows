use crate::{
    crypto::sha256_file,
    progress::SpeedEstimator,
    repository::{CatalogRepository, RepositoryError},
    telegram::{call, TelegramService},
};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tdlib_rs::{enums as e, functions as f, types as t};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteDocument {
    pub transfer: String,
    pub message_id: i64,
    pub name: String,
    pub size: i64,
    pub date: i32,
    pub sha256: String,
    pub folder_id: Option<String>,
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

#[cfg(any(test, target_os = "android"))]
#[derive(Serialize, Deserialize)]
struct AndroidDestination {
    tree: String,
    name: String,
    policy: String,
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

    pub fn record_remote(&self, doc: &RemoteDocument) -> Result<(), RepositoryError> {
        self.record_remote_batch(std::slice::from_ref(doc))
    }

    fn record_remote_batch(&self, documents: &[RemoteDocument]) -> Result<(), RepositoryError> {
        let mut c = self.connection.lock().expect("catalog");
        let tx = c.transaction()?;
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
            tx.execute(
            "INSERT INTO files (id,name,extension,kind,size_bytes,updated_at,folder,provider,telegram_message_id)
             VALUES (?1,?2,?3,?4,?5,strftime('%Y-%m-%dT%H:%M:%SZ',?6,'unixepoch'),'Mensajes guardados','telegram',?7)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name,extension=excluded.extension,kind=excluded.kind,size_bytes=excluded.size_bytes,updated_at=excluded.updated_at",
            params![id,name,ext,kind,doc.size,doc.date,doc.message_id.to_string()],
        )?;
            tx.execute(
            "INSERT INTO file_locations(file_id,folder_id) VALUES (?1,CASE WHEN EXISTS(SELECT 1 FROM folders WHERE id=?2 AND trashed=0) THEN ?2 ELSE NULL END)
             ON CONFLICT(file_id) DO UPDATE SET folder_id=excluded.folder_id",
            params![id, doc.folder_id],
        )?;
            tx.execute(
                "INSERT OR REPLACE INTO remote_documents VALUES (?1,?2)",
                params![id, serde_json::to_string(doc)?],
            )?;
            tx.execute(
            "UPDATE transfers SET status='completed',progress=100,speed_label='Guardado en Telegram' WHERE id=?1 AND direction='upload'",
            [&doc.transfer],
        )?;
            tx.execute(
                "UPDATE transfer_metadata SET remote_message_id=?1,error=NULL WHERE transfer_id=?2",
                params![doc.message_id.to_string(), doc.transfer],
            )?;
            tx.execute(
            "UPDATE transfer_runtime SET phase='completed',processed_bytes=total_bytes,speed_bps=0,eta_seconds=0,updated_at=unixepoch(),completed_at=unixepoch() WHERE transfer_id=?1",
            [&doc.transfer],
        )?;
            tx.execute("DELETE FROM transfer_pending WHERE id=?1", [&doc.transfer])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn trash(&self, id: &str, trashed: bool) -> Result<(), RepositoryError> {
        self.connection.lock().expect("catalog").execute(
            "UPDATE files SET trashed=?1 WHERE id=?2",
            params![trashed, id],
        )?;
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

fn classify_extension(ext: &str) -> &'static str {
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

impl TelegramService {
    pub async fn own_chat(&self, repo: &CatalogRepository) -> Result<i64, String> {
        if !self.refresh().await?.connected {
            return Err("Conecta tu cuenta de Telegram primero".into());
        }
        let e::User::User(me) = call(f::get_me(self.client_id)).await?;
        repo.bind_account(me.id)?;
        let e::Chat::Chat(chat) =
            call(f::create_private_chat(me.id, false, self.client_id)).await?;
        Ok(chat.id)
    }

    async fn send_metadata_text(&self, chat: i64, text: String) -> Result<(), String> {
        let content = e::InputMessageContent::InputMessageText(t::InputMessageText {
            text: t::FormattedText {
                text,
                entities: vec![],
            },
            link_preview_options: None,
            clear_draft: false,
        });
        let e::Message::Message(message) = call(f::send_message(
            chat,
            None,
            None,
            None,
            content,
            self.client_id,
        ))
        .await?;
        if message.sending_state.is_none() {
            return Ok(());
        }
        let pending = message.id;
        let deadline = Instant::now() + Duration::from_secs(25);
        while Instant::now() < deadline {
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

    pub async fn sync_catalog(&self, repo: &CatalogRepository) -> Result<usize, String> {
        let _gate = self.catalog_sync_gate.lock().await;
        let mut progress = crate::progress::SyncRun::new(&self.sync_progress);
        let result = self.sync_catalog_inner(repo, &progress).await;
        progress.finish(result.as_ref().err().cloned());
        if let Err(ref err) = result {
            let _ = crate::mobile::update_sync_notification(&serde_json::json!({
                "active": false,
                "error": err,
                "phase": "error"
            }));
        }
        result
    }

    async fn sync_catalog_inner(
        &self,
        repo: &CatalogRepository,
        progress: &crate::progress::SyncRun<'_>,
    ) -> Result<usize, String> {
        let chat = self.own_chat(repo).await?;
        let total = match tokio::time::timeout(
            Duration::from_secs(5),
            call(f::get_chat_message_count(
                chat,
                None,
                e::SearchMessagesFilter::Empty,
                false,
                self.client_id,
            )),
        )
        .await
        {
            Ok(Ok(e::Count::Count(count))) => usize::try_from(count.count).ok(),
            _ => None,
        };
        let mut scanned = 0;
        progress.scanned(scanned, total);
        let _ = crate::mobile::update_sync_notification(&serde_json::json!({
            "active": true,
            "scanned": scanned,
            "total": total,
            "percent": null,
            "phase": "scanning"
        }));
        let mut cursor = 0;
        let mut documents = Vec::new();
        let mut latest_folder_events = std::collections::HashMap::<String, FolderEvent>::new();
        let mut latest_moves = std::collections::HashMap::<i64, Option<String>>::new();
        loop {
            let e::Messages::Messages(page) = call(f::get_chat_history(
                chat,
                cursor,
                0,
                100,
                false,
                self.client_id,
            ))
            .await?;
            let mut last = cursor;
            let mut found = false;
            let mut page_documents = Vec::new();
            let mut has_new_folders = false;
            for msg in page.messages.into_iter().flatten() {
                if msg.id == cursor {
                    continue;
                }
                found = true;
                scanned += 1;
                last = msg.id;
                if let Some(event) = folder_event(&msg) {
                    let is_new = match latest_folder_events.entry(event.id.clone()) {
                        std::collections::hash_map::Entry::Vacant(v) => {
                            v.insert(event);
                            true
                        }
                        std::collections::hash_map::Entry::Occupied(_) => false,
                    };
                    if is_new {
                        has_new_folders = true;
                    }
                } else if let Some(event) = file_move_event(&msg) {
                    for message_id in event.message_ids {
                        latest_moves
                            .entry(message_id)
                            .or_insert_with(|| event.folder_id.clone());
                    }
                } else if let Some(doc) = document(&msg) {
                    page_documents.push(doc.clone());
                    documents.push(doc);
                }
            }
            if has_new_folders {
                let _ = repo.apply_folder_snapshot(latest_folder_events.values().cloned().collect());
            }
            if !page_documents.is_empty() {
                for doc in &mut page_documents {
                    if let Some(moved) = latest_moves.get(&doc.message_id) {
                        doc.folder_id = moved.clone();
                    }
                }
                let _ = repo.record_remote_batch(&page_documents);
            }
            progress.scanned(scanned, total);
            let percent = total.filter(|n| *n > 0).map(|n| ((scanned as f64 / n as f64 * 90.0) as u8).min(89));
            let _ = crate::mobile::update_sync_notification(&serde_json::json!({
                "active": true,
                "scanned": scanned,
                "total": total,
                "percent": percent,
                "phase": "scanning"
            }));
            if !found || last == cursor {
                break;
            }
            cursor = last;
            tokio::time::sleep(Duration::from_millis(60)).await;
        }

        progress.applying(0, documents.len());
        repo.apply_folder_snapshot(latest_folder_events.into_values().collect())?;

        let count = documents.len();
        // Reconcile all documents with final folder hierarchy and moves
        for doc in &documents {
            let target_folder = latest_moves
                .get(&doc.message_id)
                .cloned()
                .unwrap_or_else(|| doc.folder_id.clone());
            let file_id = format!("tg-{}", doc.message_id);
            if repo.remote(&file_id).is_ok() {
                let valid_folder = target_folder
                    .filter(|id| repo.folder_by_id(id).is_ok_and(|folder| !folder.trashed));
                let _ = repo.set_file_folder_local(&[file_id], valid_folder.as_deref());
            }
        }
        progress.applying(count, count);
        let _ = crate::mobile::update_sync_notification(&serde_json::json!({
            "active": false,
            "scanned": scanned,
            "total": total,
            "percent": 100,
            "phase": "complete"
        }));
        Ok(count)
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
        call(f::delete_messages(chat, message_ids, true, self.client_id)).await?;
        repo.remove_catalog_files(&unique_ids)
            .map_err(|e| e.to_string())
    }

    pub async fn run_upload(&self, repo: &CatalogRepository, job: &WorkItem) -> Result<(), String> {
        let chat = self.own_chat(repo).await?;
        let mut pending = job.pending.unwrap_or(0);

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
            let path = PathBuf::from(&job.path);
            let expected = job.sha256.clone();
            let expected_size = job.size;
            tauri::async_runtime::spawn_blocking(move || {
                if fs::metadata(&path).map_err(|e| e.to_string())?.len() != expected_size as u64
                    || sha256_file(&path).map_err(|e| e.to_string())? != expected
                {
                    return Err(
                        "El archivo cambió desde su selección. Selecciónalo de nuevo.".to_string(),
                    );
                }
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())??;

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
                self.client_id,
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
            if let Some(doc) = document(&message) {
                repo.record_remote(&doc).map_err(|e| e.to_string())?;
                return Ok(());
            }
        }

        let deadline = Instant::now() + Duration::from_secs(3600);
        let mut estimator = SpeedEstimator::new(0);
        while Instant::now() < deadline {
            let control = repo.transfer_control(&job.id).map_err(|e| e.to_string())?;
            if control.pause_requested || control.cancel_requested {
                if pending > 0 {
                    let _ = call(f::delete_messages(
                        chat,
                        vec![pending],
                        true,
                        self.client_id,
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

            match call(f::get_message(chat, pending, self.client_id)).await {
                Ok(e::Message::Message(message)) => {
                    if let Some(doc) = document(&message) {
                        repo.record_remote(&doc).map_err(|e| e.to_string())?;
                        return Ok(());
                    }
                    if let Some(e::MessageSendingState::Failed(_)) = message.sending_state {
                        repo.clear_pending(&job.id).map_err(|e| e.to_string())?;
                        return Err("Telegram rechazó la subida. Revisa la conexión o el tamaño y reintenta.".into());
                    }
                    if let e::MessageContent::MessageDocument(content) = message.content {
                        if let Ok(e::File::File(file)) =
                            call(f::get_file(content.document.document.id, self.client_id)).await
                        {
                            repo.set_td_file_id(&job.id, file.id)
                                .map_err(|e| e.to_string())?;
                            let uploaded = file.remote.uploaded_size.clamp(0, job.size.max(0));
                            let speed = estimator.update(uploaded);
                            let eta = SpeedEstimator::eta(job.size, uploaded, speed);
                            let phase = if uploaded >= job.size && job.size > 0 {
                                "confirming"
                            } else {
                                "uploading"
                            };
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
                        }
                    }
                }
                Err(_) => {
                    self.sync_catalog(repo).await?;
                    if repo
                        .list_transfers()
                        .map_err(|e| e.to_string())?
                        .iter()
                        .any(|t| t.id == job.id && t.status == "completed")
                    {
                        return Ok(());
                    }
                    return Err("No se pudo confirmar la subida anterior. Nuvio la reintentará sin perder el resto de la cola.".into());
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
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
            call(f::get_message(chat, message_id, self.client_id)).await?;
        let e::MessageContent::MessageDocument(content) = message.content else {
            return Err("El mensaje ya no contiene el archivo".into());
        };
        let file_id = content.document.document.id;
        repo.set_td_file_id(&job.id, file_id)
            .map_err(|e| e.to_string())?;
        call(f::download_file(file_id, 16, 0, 0, false, self.client_id)).await?;
        let deadline = Instant::now() + Duration::from_secs(3600);
        let mut estimator = SpeedEstimator::new(0);

        while Instant::now() < deadline {
            let control = repo.transfer_control(&job.id).map_err(|e| e.to_string())?;
            if control.pause_requested || control.cancel_requested {
                let _ = call(f::cancel_download_file(file_id, false, self.client_id)).await;
                if control.cancel_requested {
                    repo.mark_cancelled(&job.id).map_err(|e| e.to_string())?;
                } else {
                    repo.mark_paused(&job.id).map_err(|e| e.to_string())?;
                }
                return Ok(());
            }

            let e::File::File(file) = call(f::get_file(file_id, self.client_id)).await?;
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
            tokio::time::sleep(Duration::from_secs(1)).await;
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
    use std::io::Write;
    if target.exists() {
        return Err("El destino ya existe; Nuvio no sobrescribe archivos.".into());
    }
    let parent = target.parent().ok_or("Destino inválido")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    let mut input = fs::File::open(source).map_err(|e| e.to_string())?;
    let copied = std::io::copy(&mut input, &mut tmp).map_err(|e| e.to_string())?;
    tmp.flush().map_err(|e| e.to_string())?;
    if copied != size as u64 || sha256_file(tmp.path()).map_err(|e| e.to_string())? != expected_hash
    {
        return Err(
            "La descarga no coincide con el original. No se guardó una copia dañada.".into(),
        );
    }
    tmp.as_file().sync_all().map_err(|e| e.to_string())?;
    tmp.persist_noclobber(target).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_document(message_id: i64, name: &str) -> RemoteDocument {
        RemoteDocument {
            transfer: format!("transfer-{message_id}"),
            message_id,
            name: name.into(),
            size: 3,
            date: 1,
            sha256: "a".repeat(64),
            folder_id: None,
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
        repo.trash("tg-1", true).unwrap();
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
