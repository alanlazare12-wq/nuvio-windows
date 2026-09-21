use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use rusqlite::{params, types::Value, Connection, OptionalExtension};

use crate::domain::{AppSettings, CatalogPage, CloudFile, CloudFolder, QueueSummary, TransferJob};

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub struct CatalogRepository {
    /// Single writer connection. All catalog mutations remain serialized here.
    pub(crate) connection: Mutex<Connection>,
    /// Independent WAL readers keep dashboard/catalog queries from blocking the writer.
    readers: Vec<Mutex<Connection>>,
    next_reader: AtomicUsize,
    /// Latest high-frequency transfer telemetry. Durable SQLite checkpoints are
    /// intentionally coalesced; control/status transitions are still persisted immediately.
    live_runtime: Mutex<HashMap<String, LiveTransferRuntime>>,
    /// One-shot trust tokens for private staging files verified in this process.
    /// Tokens never survive a restart, so crash recovery still re-hashes before upload.
    verified_staging: Mutex<HashMap<String, VerifiedStaging>>,
}

#[derive(Debug, Clone)]
struct VerifiedStaging {
    path: String,
    size_bytes: i64,
    modified: Option<SystemTime>,
}

#[derive(Debug, Clone)]
struct LiveTransferRuntime {
    status: String,
    phase: String,
    progress: u8,
    processed_bytes: i64,
    total_bytes: i64,
    speed_bps: i64,
    eta_seconds: Option<i64>,
    speed_label: String,
    error: Option<String>,
    last_persisted: Instant,
}

#[derive(Debug, Clone, Copy)]
pub struct TransferControl {
    pub pause_requested: bool,
    pub cancel_requested: bool,
}

#[derive(Debug, Clone)]
pub struct UploadSourceCleanup {
    pub source_path: String,
    pub size_bytes: i64,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct UploadSourceCleanupCandidate {
    pub transfer_id: String,
    pub source_path: String,
    pub size_bytes: i64,
}

#[derive(Debug, Clone)]
pub struct CatalogDelta {
    pub cursor: i64,
    pub files: Vec<CloudFile>,
    pub removed_ids: Vec<String>,
    pub folders_changed: bool,
}

#[derive(Debug, Clone)]
pub struct CatalogPageQuery {
    pub section: String,
    pub folder_id: Option<String>,
    pub kind: Option<String>,
    pub search: String,
    pub tag: Option<String>,
    pub sort: String,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct MediaCacheEntry {
    pub path: String,
    pub size_bytes: i64,
    pub sha256: String,
    pub modified_ns: i64,
    pub verified: bool,
}

impl CatalogRepository {
    pub fn open(path: &Path) -> Result<Self, RepositoryError> {
        let connection = Connection::open(path)?;
        configure_connection(&connection, false)?;

        let mut repository = Self {
            connection: Mutex::new(connection),
            readers: Vec::new(),
            next_reader: AtomicUsize::new(0),
            live_runtime: Mutex::new(HashMap::new()),
            verified_staging: Mutex::new(HashMap::new()),
        };
        repository.migrate()?;
        repository.remove_demo()?;

        // WAL only provides reader/writer concurrency when callers don't funnel every
        // operation through the same Connection. Keep a tiny read pool: enough to
        // isolate dashboard, sync-delta and metadata reads without creating excessive
        // SQLite handles on Android.
        let reader_count = if cfg!(target_os = "android") { 2 } else { 3 };
        repository.readers.reserve(reader_count);
        for _ in 0..reader_count {
            let reader = Connection::open(path)?;
            configure_connection(&reader, true)?;
            repository.readers.push(Mutex::new(reader));
        }

        repository
            .connection
            .lock()
            .expect("catalog mutex poisoned")
            .execute_batch("PRAGMA optimize=0x10002;")?;
        Ok(repository)
    }

    fn reader(&self) -> MutexGuard<'_, Connection> {
        if self.readers.is_empty() {
            return self.connection.lock().expect("catalog mutex poisoned");
        }
        let index = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        self.readers[index]
            .lock()
            .expect("catalog reader mutex poisoned")
    }

    pub fn optimize(&self) -> Result<(), RepositoryError> {
        self.connection
            .lock()
            .expect("catalog mutex poisoned")
            .execute_batch("PRAGMA optimize;")?;
        Ok(())
    }

    fn migrate(&self) -> Result<(), RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS files (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                extension TEXT NOT NULL,
                kind TEXT NOT NULL,
                size_bytes INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL,
                favorite INTEGER NOT NULL DEFAULT 0,
                trashed INTEGER NOT NULL DEFAULT 0,
                folder TEXT NOT NULL DEFAULT '',
                tags_json TEXT NOT NULL DEFAULT '[]',
                provider TEXT NOT NULL DEFAULT 'telegram',
                telegram_message_id TEXT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_files_updated_at ON files(updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_files_kind ON files(kind);
            CREATE INDEX IF NOT EXISTS idx_files_favorite ON files(favorite);
            CREATE INDEX IF NOT EXISTS idx_files_trashed ON files(trashed);
            CREATE INDEX IF NOT EXISTS idx_files_provider_message_numeric
                ON files(provider, CAST(telegram_message_id AS INTEGER));
            CREATE INDEX IF NOT EXISTS idx_files_active_updated
                ON files(trashed, updated_at DESC, id);
            CREATE INDEX IF NOT EXISTS idx_files_active_favorite
                ON files(trashed, favorite, updated_at DESC, id);
            CREATE INDEX IF NOT EXISTS idx_files_active_kind_updated
                ON files(trashed, kind, updated_at DESC, id);
            CREATE INDEX IF NOT EXISTS idx_files_active_size
                ON files(trashed, size_bytes DESC, id);
            CREATE INDEX IF NOT EXISTS idx_files_active_name
                ON files(trashed, name COLLATE NOCASE, id);
            CREATE INDEX IF NOT EXISTS idx_files_active_name_natural
                ON files(trashed, name COLLATE NUVIO_NATURAL, id);

            CREATE TABLE IF NOT EXISTS folders (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                parent_id TEXT NULL,
                trashed INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
                FOREIGN KEY (parent_id) REFERENCES folders(id) ON DELETE SET NULL
            );
            CREATE INDEX IF NOT EXISTS idx_folders_parent ON folders(parent_id);
            CREATE INDEX IF NOT EXISTS idx_folders_trashed ON folders(trashed);

            CREATE TABLE IF NOT EXISTS file_locations (
                file_id TEXT PRIMARY KEY NOT NULL,
                folder_id TEXT NULL,
                FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE,
                FOREIGN KEY (folder_id) REFERENCES folders(id) ON DELETE SET NULL
            );
            CREATE INDEX IF NOT EXISTS idx_file_locations_folder ON file_locations(folder_id);

            CREATE TABLE IF NOT EXISTS transfer_folders (
                transfer_id TEXT PRIMARY KEY NOT NULL,
                folder_id TEXT NULL,
                FOREIGN KEY (transfer_id) REFERENCES transfers(id) ON DELETE CASCADE,
                FOREIGN KEY (folder_id) REFERENCES folders(id) ON DELETE SET NULL
            );

            CREATE TABLE IF NOT EXISTS transfers (
                id TEXT PRIMARY KEY NOT NULL,
                file_name TEXT NOT NULL,
                direction TEXT NOT NULL,
                progress INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL,
                speed_label TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_transfers_status ON transfers(status);
            CREATE INDEX IF NOT EXISTS idx_transfers_direction_status ON transfers(direction,status);

            CREATE TABLE IF NOT EXISTS transfer_metadata (
                transfer_id TEXT PRIMARY KEY NOT NULL,
                local_path TEXT NOT NULL,
                source_path TEXT NULL,
                size_bytes INTEGER NOT NULL DEFAULT 0,
                sha256 TEXT NOT NULL,
                encrypted INTEGER NOT NULL DEFAULT 0,
                remote_message_id TEXT NULL,
                delete_source_after_upload INTEGER NOT NULL DEFAULT 0,
                source_deleted INTEGER NOT NULL DEFAULT 0,
                source_delete_error TEXT NULL,
                error TEXT NULL,
                created_at INTEGER NOT NULL,
                FOREIGN KEY (transfer_id) REFERENCES transfers(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_transfer_metadata_sha256 ON transfer_metadata(sha256);

            CREATE TABLE IF NOT EXISTS transfer_runtime (
                transfer_id TEXT PRIMARY KEY NOT NULL,
                phase TEXT NOT NULL DEFAULT 'waiting',
                processed_bytes INTEGER NOT NULL DEFAULT 0,
                total_bytes INTEGER NOT NULL DEFAULT 0,
                speed_bps INTEGER NOT NULL DEFAULT 0,
                eta_seconds INTEGER NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                max_attempts INTEGER NOT NULL DEFAULT 5,
                next_retry_at INTEGER NULL,
                pause_requested INTEGER NOT NULL DEFAULT 0,
                cancel_requested INTEGER NOT NULL DEFAULT 0,
                td_file_id INTEGER NULL,
                started_at INTEGER NULL,
                updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
                completed_at INTEGER NULL,
                FOREIGN KEY (transfer_id) REFERENCES transfers(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_transfer_runtime_retry ON transfer_runtime(next_retry_at);

            CREATE TABLE IF NOT EXISTS transfer_history_changes (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                transfer_id TEXT NOT NULL,
                changed_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE TRIGGER IF NOT EXISTS trg_transfer_history_insert
            AFTER INSERT ON transfers
            WHEN NEW.status IN ('completed','duplicate','cancelled')
            BEGIN
                INSERT INTO transfer_history_changes(transfer_id) VALUES (NEW.id);
            END;
            CREATE TRIGGER IF NOT EXISTS trg_transfer_history_status
            AFTER UPDATE OF status ON transfers
            WHEN OLD.status IS NOT NEW.status
              AND (OLD.status IN ('completed','duplicate','cancelled')
                   OR NEW.status IN ('completed','duplicate','cancelled'))
            BEGIN
                INSERT INTO transfer_history_changes(transfer_id) VALUES (NEW.id);
            END;
            CREATE TRIGGER IF NOT EXISTS trg_transfer_history_delete
            AFTER DELETE ON transfers
            WHEN OLD.status IN ('completed','duplicate','cancelled')
            BEGIN
                INSERT INTO transfer_history_changes(transfer_id) VALUES (OLD.id);
            END;
            CREATE TRIGGER IF NOT EXISTS trg_transfer_history_metadata
            AFTER UPDATE ON transfer_metadata
            WHEN EXISTS (
                SELECT 1 FROM transfers t
                WHERE t.id=NEW.transfer_id
                  AND t.status IN ('completed','duplicate','cancelled')
            )
            BEGIN
                INSERT INTO transfer_history_changes(transfer_id) VALUES (NEW.transfer_id);
            END;
            CREATE TRIGGER IF NOT EXISTS trg_transfer_history_runtime
            AFTER UPDATE ON transfer_runtime
            WHEN EXISTS (
                SELECT 1 FROM transfers t
                WHERE t.id=NEW.transfer_id
                  AND t.status IN ('completed','duplicate','cancelled')
            )
            BEGIN
                INSERT INTO transfer_history_changes(transfer_id) VALUES (NEW.transfer_id);
            END;

            CREATE TABLE IF NOT EXISTS uploaded_sources (
                transfer_id TEXT PRIMARY KEY NOT NULL,
                file_name TEXT NOT NULL,
                source_path TEXT NOT NULL,
                size_bytes INTEGER NOT NULL DEFAULT 0,
                sha256 TEXT NOT NULL,
                remote_message_id TEXT NOT NULL,
                source_deleted INTEGER NOT NULL DEFAULT 0,
                source_delete_error TEXT NULL,
                created_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE INDEX IF NOT EXISTS idx_uploaded_sources_deleted ON uploaded_sources(source_deleted);

            CREATE TABLE IF NOT EXISTS app_meta (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS app_settings (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            INSERT OR IGNORE INTO app_settings VALUES ('preparation_concurrency','4');
            INSERT OR IGNORE INTO app_settings VALUES ('upload_concurrency','4');
            UPDATE app_settings SET value='8' WHERE key='upload_concurrency' AND value='4'
            AND NOT EXISTS (SELECT 1 FROM app_meta WHERE key='upload_concurrency_v2');
            INSERT OR IGNORE INTO app_meta VALUES ('upload_concurrency_v2','1');
            -- v2 raised the default to 8. With multi-GB split archives that starts too
            -- many TDLib uploads at once and produces long per-file stalls. Roll the
            -- auto-migrated/default value back once; users can still choose 8/12/16 later.
            UPDATE app_settings SET value='4' WHERE key='upload_concurrency' AND value='8'
            AND NOT EXISTS (SELECT 1 FROM app_meta WHERE key='upload_concurrency_v3');
            INSERT OR IGNORE INTO app_meta VALUES ('upload_concurrency_v3','1');
            INSERT OR IGNORE INTO app_settings VALUES ('download_concurrency','2');
            INSERT OR IGNORE INTO app_settings VALUES ('cache_limit_bytes','2147483648');
            INSERT OR IGNORE INTO app_settings VALUES ('remember_session','0');
            INSERT OR IGNORE INTO app_settings VALUES ('conflict_policy','skip');
            INSERT OR IGNORE INTO app_settings VALUES ('delete_original_after_upload','0');
            INSERT OR IGNORE INTO app_settings VALUES ('speed_limit_bps','');
            INSERT OR IGNORE INTO app_settings VALUES ('resource_profile','balanced');

            CREATE TABLE IF NOT EXISTS media_cache_entries (
                path TEXT PRIMARY KEY NOT NULL,
                file_id TEXT NULL,
                size_bytes INTEGER NOT NULL DEFAULT 0,
                sha256 TEXT NOT NULL DEFAULT '',
                modified_ns INTEGER NOT NULL DEFAULT 0,
                verified INTEGER NOT NULL DEFAULT 0,
                last_access INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE INDEX IF NOT EXISTS idx_media_cache_file_id
                ON media_cache_entries(file_id);
            CREATE INDEX IF NOT EXISTS idx_media_cache_lru
                ON media_cache_entries(last_access, path);

            CREATE TABLE IF NOT EXISTS catalog_changes (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                entity TEXT NOT NULL CHECK(entity IN ('file','folder')),
                entity_id TEXT NOT NULL,
                operation TEXT NOT NULL CHECK(operation IN ('upsert','delete')),
                changed_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE INDEX IF NOT EXISTS idx_catalog_changes_seq_entity
                ON catalog_changes(seq, entity);
            CREATE INDEX IF NOT EXISTS idx_catalog_changes_entity_id_seq
                ON catalog_changes(entity, entity_id, seq DESC);

            CREATE TRIGGER IF NOT EXISTS trg_catalog_files_insert
            AFTER INSERT ON files BEGIN
                INSERT INTO catalog_changes(entity,entity_id,operation)
                VALUES ('file',NEW.id,'upsert');
            END;
            CREATE TRIGGER IF NOT EXISTS trg_catalog_files_update
            AFTER UPDATE ON files
            WHEN OLD.name IS NOT NEW.name
              OR OLD.extension IS NOT NEW.extension
              OR OLD.kind IS NOT NEW.kind
              OR OLD.size_bytes IS NOT NEW.size_bytes
              OR OLD.updated_at IS NOT NEW.updated_at
              OR OLD.favorite IS NOT NEW.favorite
              OR OLD.trashed IS NOT NEW.trashed
              OR OLD.folder IS NOT NEW.folder
              OR OLD.tags_json IS NOT NEW.tags_json
              OR OLD.provider IS NOT NEW.provider
              OR OLD.telegram_message_id IS NOT NEW.telegram_message_id
            BEGIN
                INSERT INTO catalog_changes(entity,entity_id,operation)
                VALUES ('file',NEW.id,'upsert');
            END;
            CREATE TRIGGER IF NOT EXISTS trg_catalog_files_delete
            AFTER DELETE ON files BEGIN
                INSERT INTO catalog_changes(entity,entity_id,operation)
                VALUES ('file',OLD.id,'delete');
            END;
            CREATE TRIGGER IF NOT EXISTS trg_catalog_locations_insert
            AFTER INSERT ON file_locations BEGIN
                INSERT INTO catalog_changes(entity,entity_id,operation)
                VALUES ('file',NEW.file_id,'upsert');
            END;
            CREATE TRIGGER IF NOT EXISTS trg_catalog_locations_update
            AFTER UPDATE ON file_locations
            WHEN OLD.folder_id IS NOT NEW.folder_id
            BEGIN
                INSERT INTO catalog_changes(entity,entity_id,operation)
                VALUES ('file',NEW.file_id,'upsert');
            END;
            CREATE TRIGGER IF NOT EXISTS trg_catalog_locations_delete
            AFTER DELETE ON file_locations BEGIN
                INSERT INTO catalog_changes(entity,entity_id,operation)
                VALUES ('file',OLD.file_id,'upsert');
            END;
            CREATE TRIGGER IF NOT EXISTS trg_catalog_folders_insert
            AFTER INSERT ON folders BEGIN
                INSERT INTO catalog_changes(entity,entity_id,operation)
                VALUES ('folder',NEW.id,'upsert');
            END;
            CREATE TRIGGER IF NOT EXISTS trg_catalog_folders_update
            AFTER UPDATE ON folders
            WHEN OLD.name IS NOT NEW.name
              OR OLD.parent_id IS NOT NEW.parent_id
              OR OLD.trashed IS NOT NEW.trashed
              OR OLD.updated_at IS NOT NEW.updated_at
            BEGIN
                INSERT INTO catalog_changes(entity,entity_id,operation)
                VALUES ('folder',NEW.id,'upsert');
            END;
            CREATE TRIGGER IF NOT EXISTS trg_catalog_folders_delete
            AFTER DELETE ON folders BEGIN
                INSERT INTO catalog_changes(entity,entity_id,operation)
                VALUES ('folder',OLD.id,'delete');
            END;

            INSERT OR IGNORE INTO transfer_runtime (transfer_id,phase,total_bytes,updated_at)
            SELECT t.id,
                   CASE t.status
                     WHEN 'completed' THEN 'completed'
                     WHEN 'failed' THEN 'error'
                     WHEN 'paused' THEN 'paused'
                     WHEN 'duplicate' THEN 'duplicate'
                     ELSE 'waiting'
                   END,
                   COALESCE(m.size_bytes,0),
                   unixepoch()
            FROM transfers t
            LEFT JOIN transfer_metadata m ON m.transfer_id=t.id;
            "#,
        )?;
        ensure_column(
            &connection,
            "transfer_metadata",
            "source_path",
            "source_path TEXT NULL",
        )?;
        ensure_column(
            &connection,
            "transfer_metadata",
            "delete_source_after_upload",
            "delete_source_after_upload INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &connection,
            "transfer_metadata",
            "source_deleted",
            "source_deleted INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &connection,
            "transfer_metadata",
            "source_delete_error",
            "source_delete_error TEXT NULL",
        )?;
        connection.execute_batch(
            r#"
            INSERT OR IGNORE INTO uploaded_sources(
                transfer_id,file_name,source_path,size_bytes,sha256,remote_message_id,
                source_deleted,source_delete_error,created_at
            )
            SELECT t.id,t.file_name,m.source_path,m.size_bytes,m.sha256,m.remote_message_id,
                   COALESCE(m.source_deleted,0),m.source_delete_error,m.created_at
            FROM transfers t
            JOIN transfer_metadata m ON m.transfer_id=t.id
            WHERE t.direction='upload' AND t.status='completed'
              AND m.remote_message_id IS NOT NULL AND m.remote_message_id<>''
              AND m.source_path IS NOT NULL AND m.source_path<>'' AND m.sha256<>'';

            DROP INDEX IF EXISTS idx_files_active_updated;
            DROP INDEX IF EXISTS idx_files_active_favorite;
            CREATE INDEX idx_files_active_updated
                ON files(trashed,updated_at DESC,id);
            CREATE INDEX idx_files_active_favorite
                ON files(trashed,favorite,updated_at DESC,id);
            "#,
        )?;
        let _ = initialize_catalog_fts(&connection);
        Ok(())
    }

    fn remove_demo(&self) -> Result<(), RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        connection.execute_batch(
            "DELETE FROM files WHERE id IN ('file-001','file-002','file-003','file-004','file-005','file-006') AND telegram_message_id IS NULL;
             DELETE FROM transfers WHERE id IN ('transfer-001','transfer-002') AND NOT EXISTS (SELECT 1 FROM transfer_metadata WHERE transfer_id=transfers.id);
             UPDATE transfers SET status='paused',speed_label='Interrumpida: lista para reanudar'
             WHERE status IN ('running','analyzing','copying','uploading','downloading','confirming');
             UPDATE transfer_runtime SET phase='paused',pause_requested=0,cancel_requested=0,speed_bps=0,eta_seconds=NULL,updated_at=unixepoch()
             WHERE transfer_id IN (SELECT id FROM transfers WHERE status='paused');",
        )?;
        Ok(())
    }

    pub fn list_files(&self) -> Result<Vec<CloudFile>, RepositoryError> {
        let connection = self.reader();
        let mut statement = connection.prepare(
            r#"WITH RECURSIVE folder_paths(id, path) AS (
                   SELECT id, name FROM folders WHERE parent_id IS NULL
                   UNION ALL
                   SELECT child.id, parent.path || ' / ' || child.name
                   FROM folders child JOIN folder_paths parent ON child.parent_id = parent.id
               )
               SELECT f.id, f.name, f.extension, f.kind, f.size_bytes, f.updated_at,
                      f.favorite, f.trashed, COALESCE(folder_paths.path, 'Mi unidad'),
                      fl.folder_id, f.tags_json, f.provider, f.telegram_message_id
               FROM files f
               LEFT JOIN file_locations fl ON fl.file_id = f.id
               LEFT JOIN folder_paths ON folder_paths.id = fl.folder_id
               ORDER BY f.updated_at DESC"#,
        )?;

        let rows = statement.query_map([], |row| {
            let tags_json: String = row.get(10)?;
            let tags = if tags_json.is_empty() || tags_json == "[]" {
                Vec::new()
            } else {
                serde_json::from_str::<Vec<String>>(&tags_json).unwrap_or_default()
            };
            Ok(CloudFile {
                id: row.get(0)?,
                name: row.get(1)?,
                extension: row.get(2)?,
                kind: row.get(3)?,
                size_bytes: row.get(4)?,
                updated_at: row.get(5)?,
                favorite: row.get::<_, i64>(6)? != 0,
                trashed: row.get::<_, i64>(7)? != 0,
                folder: row.get(8)?,
                folder_id: row.get(9)?,
                tags,
                provider: row.get(11)?,
                telegram_message_id: row.get(12)?,
            })
        })?;

        let mut files = Vec::new();
        for row in rows {
            files.push(row?);
        }
        Ok(files)
    }

    pub fn list_files_page(&self, query: CatalogPageQuery) -> Result<CatalogPage, RepositoryError> {
        let connection = self.reader();
        let limit = query.limit.clamp(1, 200);
        let offset = query.offset.min(10_000_000);
        let search = query.search.trim().to_lowercase();
        let mut predicates = Vec::<String>::new();
        let mut values = Vec::<Value>::new();

        if query.section == "trash" {
            predicates.push("f.trashed=1".into());
        } else {
            predicates.push("f.trashed=0".into());
        }
        if query.section == "favorites" {
            predicates.push("f.favorite=1".into());
        }
        if let Some(kind) = query
            .kind
            .as_deref()
            .filter(|kind| *kind != "all" && !kind.is_empty())
        {
            predicates.push("f.kind=?".into());
            values.push(Value::Text(kind.to_string()));
        }
        if let Some(tag) = query.tag.as_deref().filter(|tag| !tag.trim().is_empty()) {
            predicates.push(
                "EXISTS (SELECT 1 FROM json_each(f.tags_json) WHERE lower(value)=lower(?))".into(),
            );
            values.push(Value::Text(tag.trim().to_string()));
        }
        // Match the current UI semantics: when there is no search, "Mis archivos"
        // is scoped to the selected folder. A search is intentionally global.
        if query.section == "files" && search.is_empty() {
            match query.folder_id {
                Some(folder_id) => {
                    predicates.push("fl.folder_id=?".into());
                    values.push(Value::Text(folder_id));
                }
                None => predicates.push("fl.folder_id IS NULL".into()),
            }
        }
        if !search.is_empty() {
            let escaped = search
                .replace('!', "!!")
                .replace('%', "!%")
                .replace('_', "!_");
            let like = format!("%{escaped}%");
            if search.chars().count() >= 3 && catalog_fts_available(&connection) {
                let phrase = format!("\"{}\"", search.replace('"', "\"\""));
                predicates.push(
                    "(f.id IN (SELECT id FROM files_fts WHERE files_fts MATCH ?)
                       OR lower(COALESCE(folder_paths.path,'Mi unidad')) LIKE ? ESCAPE '!')"
                        .into(),
                );
                values.push(Value::Text(phrase));
                values.push(Value::Text(like));
            } else {
                predicates.push(
                    "(lower(f.name) LIKE ? ESCAPE '!'
                       OR lower(f.extension) LIKE ? ESCAPE '!'
                       OR lower(f.tags_json) LIKE ? ESCAPE '!'
                       OR lower(COALESCE(folder_paths.path,'Mi unidad')) LIKE ? ESCAPE '!')"
                        .into(),
                );
                for _ in 0..4 {
                    values.push(Value::Text(like.clone()));
                }
            }
        }

        let where_sql = predicates.join(" AND ");
        let cte = r#"WITH RECURSIVE folder_paths(id, path) AS (
            SELECT id, name FROM folders WHERE parent_id IS NULL
            UNION ALL
            SELECT child.id, parent.path || ' / ' || child.name
            FROM folders child JOIN folder_paths parent ON child.parent_id=parent.id
        )"#;
        let count_sql = format!(
            "{cte}
             SELECT COUNT(*)
             FROM files f
             LEFT JOIN file_locations fl ON fl.file_id=f.id
             LEFT JOIN folder_paths ON folder_paths.id=fl.folder_id
             WHERE {where_sql}"
        );
        let total: i64 = connection.query_row(
            &count_sql,
            rusqlite::params_from_iter(values.iter()),
            |row| row.get(0),
        )?;

        let order_sql = match query.sort.as_str() {
            "oldest" => "f.updated_at ASC,f.id ASC",
            "name" => "f.name COLLATE NUVIO_NATURAL ASC,f.id ASC",
            "size" => "f.size_bytes DESC,f.id ASC",
            _ => "f.updated_at DESC,f.id ASC",
        };
        let data_sql = format!(
            "{cte}
             SELECT f.id,f.name,f.extension,f.kind,f.size_bytes,f.updated_at,
                    f.favorite,f.trashed,COALESCE(folder_paths.path,'Mi unidad'),
                    fl.folder_id,f.tags_json,f.provider,f.telegram_message_id
             FROM files f
             LEFT JOIN file_locations fl ON fl.file_id=f.id
             LEFT JOIN folder_paths ON folder_paths.id=fl.folder_id
             WHERE {where_sql}
             ORDER BY {order_sql}
             LIMIT ? OFFSET ?"
        );
        let mut data_values = values;
        data_values.push(Value::Integer(limit as i64));
        data_values.push(Value::Integer(offset as i64));
        let mut statement = connection.prepare(&data_sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(data_values.iter()), |row| {
            let tags_json: String = row.get(10)?;
            let tags = if tags_json.is_empty() || tags_json == "[]" {
                Vec::new()
            } else {
                serde_json::from_str::<Vec<String>>(&tags_json).unwrap_or_default()
            };
            Ok(CloudFile {
                id: row.get(0)?,
                name: row.get(1)?,
                extension: row.get(2)?,
                kind: row.get(3)?,
                size_bytes: row.get(4)?,
                updated_at: row.get(5)?,
                favorite: row.get::<_, i64>(6)? != 0,
                trashed: row.get::<_, i64>(7)? != 0,
                folder: row.get(8)?,
                folder_id: row.get(9)?,
                tags,
                provider: row.get(11)?,
                telegram_message_id: row.get(12)?,
            })
        })?;
        let mut files = Vec::with_capacity(limit.min(total.max(0) as usize));
        for row in rows {
            files.push(row?);
        }

        let total = total.max(0) as usize;
        Ok(CatalogPage {
            has_more: offset.saturating_add(files.len()) < total,
            files,
            total,
            offset,
            limit,
        })
    }

    pub fn catalog_stats(&self) -> Result<(i64, usize, usize, usize), RepositoryError> {
        let connection = self.reader();
        let (total_bytes, file_count, favorite_count, trash_count): (i64, i64, i64, i64) =
            connection.query_row(
                "SELECT COALESCE(SUM(CASE WHEN trashed=0 THEN size_bytes ELSE 0 END),0),
                        COUNT(CASE WHEN trashed=0 THEN 1 END),
                        COUNT(CASE WHEN trashed=0 AND favorite=1 THEN 1 END),
                        COUNT(CASE WHEN trashed=1 THEN 1 END)
                 FROM files",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        Ok((
            total_bytes.max(0),
            file_count.max(0) as usize,
            favorite_count.max(0) as usize,
            trash_count.max(0) as usize,
        ))
    }

    pub fn media_cache_entry(
        &self,
        path: &Path,
    ) -> Result<Option<MediaCacheEntry>, RepositoryError> {
        let path = path.to_string_lossy();
        self.reader()
            .query_row(
                "SELECT path,size_bytes,sha256,modified_ns,verified
                 FROM media_cache_entries WHERE path=?1",
                [path.as_ref()],
                |row| {
                    Ok(MediaCacheEntry {
                        path: row.get(0)?,
                        size_bytes: row.get(1)?,
                        sha256: row.get(2)?,
                        modified_ns: row.get(3)?,
                        verified: row.get::<_, i64>(4)? != 0,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn media_cache_reconcile_file(
        &self,
        path: &Path,
        size_bytes: i64,
        modified_ns: i64,
    ) -> Result<(), RepositoryError> {
        self.connection
            .lock()
            .expect("catalog mutex poisoned")
            .execute(
                "INSERT INTO media_cache_entries(path,size_bytes,modified_ns,verified)
                 VALUES (?1,?2,?3,0)
                 ON CONFLICT(path) DO UPDATE SET
                   size_bytes=excluded.size_bytes,
                   sha256=CASE
                     WHEN media_cache_entries.size_bytes=excluded.size_bytes
                      AND media_cache_entries.modified_ns=excluded.modified_ns
                     THEN media_cache_entries.sha256 ELSE '' END,
                   modified_ns=excluded.modified_ns,
                   verified=CASE
                     WHEN media_cache_entries.size_bytes=excluded.size_bytes
                      AND media_cache_entries.modified_ns=excluded.modified_ns
                     THEN media_cache_entries.verified ELSE 0 END",
                params![
                    path.to_string_lossy().as_ref(),
                    size_bytes.max(0),
                    modified_ns
                ],
            )?;
        Ok(())
    }

    pub fn media_cache_store_verified(
        &self,
        file_id: &str,
        path: &Path,
        size_bytes: i64,
        sha256: &str,
        modified_ns: i64,
    ) -> Result<(), RepositoryError> {
        self.connection
            .lock()
            .expect("catalog mutex poisoned")
            .execute(
                "INSERT INTO media_cache_entries(
                    path,file_id,size_bytes,sha256,modified_ns,verified,last_access
                 ) VALUES (?1,?2,?3,?4,?5,1,unixepoch())
                 ON CONFLICT(path) DO UPDATE SET
                    file_id=excluded.file_id,
                    size_bytes=excluded.size_bytes,
                    sha256=excluded.sha256,
                    modified_ns=excluded.modified_ns,
                    verified=1,
                    last_access=unixepoch()",
                params![
                    path.to_string_lossy().as_ref(),
                    file_id,
                    size_bytes.max(0),
                    sha256,
                    modified_ns
                ],
            )?;
        Ok(())
    }

    pub fn media_cache_touch(&self, path: &Path) -> Result<(), RepositoryError> {
        self.connection
            .lock()
            .expect("catalog mutex poisoned")
            .execute(
                "UPDATE media_cache_entries SET last_access=unixepoch() WHERE path=?1",
                [path.to_string_lossy().as_ref()],
            )?;
        Ok(())
    }

    pub fn media_cache_total_bytes(&self) -> Result<i64, RepositoryError> {
        self.reader()
            .query_row(
                "SELECT COALESCE(SUM(size_bytes),0) FROM media_cache_entries",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|value| value.max(0))
            .map_err(Into::into)
    }

    pub fn media_cache_lru(&self, limit: usize) -> Result<Vec<MediaCacheEntry>, RepositoryError> {
        let connection = self.reader();
        let mut statement = connection.prepare(
            "SELECT path,size_bytes,sha256,modified_ns,verified
             FROM media_cache_entries
             ORDER BY last_access ASC,path ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit.clamp(1, 4096) as i64], |row| {
            Ok(MediaCacheEntry {
                path: row.get(0)?,
                size_bytes: row.get(1)?,
                sha256: row.get(2)?,
                modified_ns: row.get(3)?,
                verified: row.get::<_, i64>(4)? != 0,
            })
        })?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }

    pub fn media_cache_paths(&self) -> Result<Vec<String>, RepositoryError> {
        let connection = self.reader();
        let mut statement = connection.prepare("SELECT path FROM media_cache_entries")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut paths = Vec::new();
        for row in rows {
            paths.push(row?);
        }
        Ok(paths)
    }

    pub fn media_cache_remove_path(&self, path: &Path) -> Result<(), RepositoryError> {
        self.connection
            .lock()
            .expect("catalog mutex poisoned")
            .execute(
                "DELETE FROM media_cache_entries WHERE path=?1",
                [path.to_string_lossy().as_ref()],
            )?;
        Ok(())
    }

    pub fn media_cache_clear_index(&self) -> Result<(), RepositoryError> {
        self.connection
            .lock()
            .expect("catalog mutex poisoned")
            .execute("DELETE FROM media_cache_entries", [])?;
        Ok(())
    }

    pub fn catalog_change_cursor(&self) -> Result<i64, RepositoryError> {
        self.reader()
            .query_row(
                "SELECT COALESCE(MAX(seq),0) FROM catalog_changes",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn sync_file_delta(&self, after_seq: i64) -> Result<CatalogDelta, RepositoryError> {
        let connection = self.reader();
        let cursor: i64 = connection.query_row(
            "SELECT COALESCE(MAX(seq),0) FROM catalog_changes",
            [],
            |row| row.get(0),
        )?;
        if cursor <= after_seq {
            return Ok(CatalogDelta {
                cursor,
                files: Vec::new(),
                removed_ids: Vec::new(),
                folders_changed: false,
            });
        }

        let folders_changed: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM catalog_changes WHERE seq>?1 AND entity='folder')",
            [after_seq],
            |row| row.get(0),
        )?;

        let mut statement = connection.prepare(
            r#"WITH RECURSIVE
               latest(entity_id, seq) AS (
                   SELECT entity_id, MAX(seq)
                   FROM catalog_changes
                   WHERE seq > ?1 AND entity='file'
                   GROUP BY entity_id
               ),
               changed(entity_id, seq, operation) AS (
                   SELECT c.entity_id, c.seq, c.operation
                   FROM catalog_changes c
                   JOIN latest l ON l.seq=c.seq
               ),
               folder_paths(id, path) AS (
                   SELECT id, name FROM folders WHERE parent_id IS NULL
                   UNION ALL
                   SELECT child.id, parent.path || ' / ' || child.name
                   FROM folders child JOIN folder_paths parent ON child.parent_id = parent.id
               )
               SELECT f.id, f.name, f.extension, f.kind, f.size_bytes, f.updated_at,
                      f.favorite, f.trashed, COALESCE(folder_paths.path, 'Mi unidad'),
                      fl.folder_id, f.tags_json, f.provider, f.telegram_message_id
               FROM changed c
               JOIN files f ON f.id=c.entity_id
               LEFT JOIN file_locations fl ON fl.file_id=f.id
               LEFT JOIN folder_paths ON folder_paths.id=fl.folder_id
               WHERE c.operation='upsert'
               ORDER BY c.seq ASC"#,
        )?;
        let rows = statement.query_map([after_seq], |row| {
            let tags_json: String = row.get(10)?;
            let tags = if tags_json.is_empty() || tags_json == "[]" {
                Vec::new()
            } else {
                serde_json::from_str::<Vec<String>>(&tags_json).unwrap_or_default()
            };
            Ok(CloudFile {
                id: row.get(0)?,
                name: row.get(1)?,
                extension: row.get(2)?,
                kind: row.get(3)?,
                size_bytes: row.get(4)?,
                updated_at: row.get(5)?,
                favorite: row.get::<_, i64>(6)? != 0,
                trashed: row.get::<_, i64>(7)? != 0,
                folder: row.get(8)?,
                folder_id: row.get(9)?,
                tags,
                provider: row.get(11)?,
                telegram_message_id: row.get(12)?,
            })
        })?;
        let mut files = Vec::new();
        for row in rows {
            files.push(row?);
        }
        drop(statement);

        let mut removed_statement = connection.prepare(
            r#"WITH latest(entity_id, seq) AS (
                   SELECT entity_id, MAX(seq)
                   FROM catalog_changes
                   WHERE seq > ?1 AND entity='file'
                   GROUP BY entity_id
               )
               SELECT c.entity_id
               FROM catalog_changes c
               JOIN latest l ON l.seq=c.seq
               WHERE c.operation='delete'
               ORDER BY c.seq ASC"#,
        )?;
        let removed_rows =
            removed_statement.query_map([after_seq], |row| row.get::<_, String>(0))?;
        let mut removed_ids = Vec::new();
        for row in removed_rows {
            removed_ids.push(row?);
        }

        Ok(CatalogDelta {
            cursor,
            files,
            removed_ids,
            folders_changed,
        })
    }

    pub fn list_folders(&self) -> Result<Vec<CloudFolder>, RepositoryError> {
        let connection = self.reader();
        let mut statement = connection.prepare(
            r#"WITH file_stats(folder_id, file_count, size_bytes) AS (
                   SELECT fl.folder_id, COUNT(*), COALESCE(SUM(fi.size_bytes),0)
                   FROM file_locations fl
                   JOIN files fi ON fi.id=fl.file_id
                   WHERE fi.trashed=0
                   GROUP BY fl.folder_id
               ),
               child_stats(parent_id, child_count) AS (
                   SELECT parent_id, COUNT(*)
                   FROM folders
                   WHERE trashed=0
                   GROUP BY parent_id
               )
               SELECT f.id, f.name, f.parent_id, f.trashed, f.created_at, f.updated_at,
                      COALESCE(fs.file_count,0), COALESCE(cs.child_count,0),
                      COALESCE(fs.size_bytes,0)
               FROM folders f
               LEFT JOIN file_stats fs ON fs.folder_id=f.id
               LEFT JOIN child_stats cs ON cs.parent_id=f.id
               ORDER BY lower(f.name), f.id"#,
        )?;
        let rows = statement.query_map([], |row| {
            Ok(CloudFolder {
                id: row.get(0)?,
                name: row.get(1)?,
                parent_id: row.get(2)?,
                trashed: row.get::<_, i64>(3)? != 0,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
                file_count: row.get::<_, i64>(6)?.max(0) as usize,
                child_count: row.get::<_, i64>(7)?.max(0) as usize,
                size_bytes: row.get(8)?,
            })
        })?;
        let mut folders = Vec::new();
        for row in rows {
            folders.push(row?);
        }
        Ok(folders)
    }

    pub fn validate_new_folder(
        &self,
        name: &str,
        parent_id: Option<&str>,
    ) -> Result<(), RepositoryError> {
        validate_folder_name(name)?;
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        ensure_folder_parent(&connection, parent_id, None)?;
        ensure_unique_folder_name(&connection, name, parent_id, None)
    }

    #[cfg(test)]
    pub fn create_folder_local(
        &self,
        id: &str,
        name: &str,
        parent_id: Option<&str>,
    ) -> Result<(), RepositoryError> {
        self.validate_new_folder(name, parent_id)?;
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        connection.execute(
            "INSERT INTO folders(id,name,parent_id,trashed,created_at,updated_at) VALUES (?1,?2,?3,0,unixepoch(),unixepoch())",
            params![id, name.trim(), parent_id],
        )?;
        Ok(())
    }

    pub fn apply_folder_event(
        &self,
        id: &str,
        name: &str,
        parent_id: Option<&str>,
        trashed: bool,
    ) -> Result<(), RepositoryError> {
        validate_folder_name(name)?;
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        ensure_folder_parent(&connection, parent_id, Some(id))?;
        if parent_id != Some(id) {
            connection.execute(
                "INSERT INTO folders(id,name,parent_id,trashed,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,unixepoch(),unixepoch())
                 ON CONFLICT(id) DO UPDATE SET name=excluded.name,parent_id=excluded.parent_id,trashed=excluded.trashed,updated_at=unixepoch()",
                params![id, name.trim(), parent_id, if trashed { 1 } else { 0 }],
            )?;
        }
        Ok(())
    }

    pub fn validate_folder_update(
        &self,
        id: &str,
        name: &str,
        parent_id: Option<&str>,
    ) -> Result<(), RepositoryError> {
        validate_folder_name(name)?;
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM folders WHERE id=?1)",
            [id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(RepositoryError::Database(
                rusqlite::Error::QueryReturnedNoRows,
            ));
        }
        ensure_folder_parent(&connection, parent_id, Some(id))?;
        ensure_unique_folder_name(&connection, name, parent_id, Some(id))
    }

    pub fn update_folder_local(
        &self,
        id: &str,
        name: &str,
        parent_id: Option<&str>,
        trashed: bool,
    ) -> Result<(), RepositoryError> {
        self.validate_folder_update(id, name, parent_id)?;
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM folders WHERE id=?1)",
            [id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(RepositoryError::Database(
                rusqlite::Error::QueryReturnedNoRows,
            ));
        }
        ensure_folder_parent(&connection, parent_id, Some(id))?;
        ensure_unique_folder_name(&connection, name, parent_id, Some(id))?;
        connection.execute(
            "UPDATE folders SET name=?1,parent_id=?2,trashed=?3,updated_at=unixepoch() WHERE id=?4",
            params![name.trim(), parent_id, if trashed { 1 } else { 0 }, id],
        )?;
        Ok(())
    }

    pub fn folder_by_id(&self, id: &str) -> Result<CloudFolder, RepositoryError> {
        self.list_folders()?
            .into_iter()
            .find(|folder| folder.id == id)
            .ok_or(RepositoryError::Database(
                rusqlite::Error::QueryReturnedNoRows,
            ))
    }

    pub fn folder_is_empty(&self, id: &str) -> Result<bool, RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let count: i64 = connection.query_row(
            "SELECT (SELECT COUNT(*) FROM file_locations WHERE folder_id=?1) + (SELECT COUNT(*) FROM folders WHERE parent_id=?1 AND trashed=0) + (SELECT COUNT(*) FROM transfer_folders tf JOIN transfers t ON t.id=tf.transfer_id WHERE tf.folder_id=?1 AND t.status NOT IN ('completed','duplicate','cancelled'))",
            [id],
            |row| row.get(0),
        )?;
        Ok(count == 0)
    }

    pub fn set_file_folder_local(
        &self,
        ids: &[String],
        folder_id: Option<&str>,
    ) -> Result<usize, RepositoryError> {
        let mut connection = self.connection.lock().expect("catalog mutex poisoned");
        ensure_folder_parent(&connection, folder_id, None)?;
        let transaction = connection.transaction()?;
        let mut changed = 0usize;
        for id in ids {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM files WHERE id=?1)",
                [id],
                |row| row.get(0),
            )?;
            if !exists {
                continue;
            }
            transaction.execute(
                "INSERT INTO file_locations(file_id,folder_id) VALUES (?1,?2) ON CONFLICT(file_id) DO UPDATE SET folder_id=excluded.folder_id",
                params![id, folder_id],
            )?;
            changed += 1;
        }
        transaction.commit()?;
        Ok(changed)
    }

    pub fn list_transfers(&self) -> Result<Vec<TransferJob>, RepositoryError> {
        self.list_transfers_matching("1=1", None)
    }

    pub fn list_active_transfers(&self) -> Result<Vec<TransferJob>, RepositoryError> {
        let mut transfers = self.list_transfers_matching(
            "t.status NOT IN ('completed','duplicate','cancelled')",
            None,
        )?;
        self.overlay_live_runtime(&mut transfers);
        Ok(transfers)
    }

    fn overlay_live_runtime(&self, transfers: &mut [TransferJob]) {
        let active_ids = transfers
            .iter()
            .map(|job| job.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let mut live = self
            .live_runtime
            .lock()
            .expect("live transfer runtime mutex poisoned");
        live.retain(|id, _| active_ids.contains(id.as_str()));

        for job in transfers {
            let Some(runtime) = live.get(&job.id) else {
                continue;
            };
            job.status.clone_from(&runtime.status);
            job.phase.clone_from(&runtime.phase);
            job.progress = runtime.progress;
            job.processed_bytes = runtime.processed_bytes;
            job.total_bytes = runtime.total_bytes;
            job.speed_bps = runtime.speed_bps;
            job.eta_seconds = runtime.eta_seconds;
            job.speed_label.clone_from(&runtime.speed_label);
            job.error.clone_from(&runtime.error);
            job.can_pause = matches!(
                job.status.as_str(),
                "waiting"
                    | "analyzing"
                    | "copying"
                    | "ready"
                    | "queued"
                    | "uploading"
                    | "downloading"
                    | "confirming"
                    | "running"
            );
            job.can_retry = matches!(job.status.as_str(), "failed" | "paused" | "cancelled");
            job.can_cancel =
                !matches!(job.status.as_str(), "completed" | "duplicate" | "cancelled");
        }
    }

    fn clear_live_runtime(&self, id: &str) {
        self.live_runtime
            .lock()
            .expect("live transfer runtime mutex poisoned")
            .remove(id);
    }

    fn remember_verified_staging(&self, id: &str, path: &str, size_bytes: i64) {
        let Ok(metadata) = fs::metadata(path) else {
            return;
        };
        if !metadata.is_file() || metadata.len() != size_bytes.max(0) as u64 {
            return;
        }
        self.verified_staging
            .lock()
            .expect("verified staging mutex poisoned")
            .insert(
                id.to_string(),
                VerifiedStaging {
                    path: path.to_string(),
                    size_bytes,
                    modified: metadata.modified().ok(),
                },
            );
    }

    pub(crate) fn take_verified_staging(&self, id: &str, path: &str, size_bytes: i64) -> bool {
        let Some(verified) = self
            .verified_staging
            .lock()
            .expect("verified staging mutex poisoned")
            .remove(id)
        else {
            return false;
        };
        if verified.path != path || verified.size_bytes != size_bytes {
            return false;
        }
        let Ok(metadata) = fs::metadata(path) else {
            return false;
        };
        metadata.is_file()
            && metadata.len() == size_bytes.max(0) as u64
            && metadata.modified().ok() == verified.modified
    }

    pub fn list_transfer_history(&self, limit: usize) -> Result<Vec<TransferJob>, RepositoryError> {
        self.list_transfers_matching(
            "t.status IN ('completed','duplicate','cancelled')",
            Some(limit.clamp(1, 10_000)),
        )
    }

    pub fn transfer_history_cursor(&self) -> Result<i64, RepositoryError> {
        self.reader()
            .query_row(
                "SELECT COALESCE(MAX(seq),0) FROM transfer_history_changes",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn transfer_by_id(&self, id: &str) -> Result<Option<TransferJob>, RepositoryError> {
        let connection = self.reader();
        connection
            .query_row(
                r#"SELECT t.id,t.file_name,t.direction,t.progress,t.status,t.speed_label,
                          COALESCE(r.phase,t.status),COALESCE(r.processed_bytes,0),
                          COALESCE(r.total_bytes,m.size_bytes,0),COALESCE(r.speed_bps,0),r.eta_seconds,
                          COALESCE(r.attempts,0),COALESCE(r.max_attempts,5),m.error,m.source_path,
                          COALESCE(m.source_deleted,0),m.source_delete_error,m.remote_message_id,r.started_at,
                          COALESCE(r.updated_at,m.created_at,0)
                   FROM transfers t
                   LEFT JOIN transfer_metadata m ON m.transfer_id=t.id
                   LEFT JOIN transfer_runtime r ON r.transfer_id=t.id
                   WHERE t.id=?1
                   LIMIT 1"#,
                [id],
                transfer_job_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    fn list_transfers_matching(
        &self,
        predicate: &str,
        limit: Option<usize>,
    ) -> Result<Vec<TransferJob>, RepositoryError> {
        let limit_clause = limit
            .map(|value| format!(" LIMIT {value}"))
            .unwrap_or_default();
        let sql = format!(
            r#"SELECT t.id,t.file_name,t.direction,t.progress,t.status,t.speed_label,
                      COALESCE(r.phase,t.status),COALESCE(r.processed_bytes,0),
                      COALESCE(r.total_bytes,m.size_bytes,0),COALESCE(r.speed_bps,0),r.eta_seconds,
                      COALESCE(r.attempts,0),COALESCE(r.max_attempts,5),m.error,m.source_path,
                      COALESCE(m.source_deleted,0),m.source_delete_error,m.remote_message_id,r.started_at,
                      COALESCE(r.updated_at,m.created_at,0)
               FROM transfers t
               LEFT JOIN transfer_metadata m ON m.transfer_id=t.id
               LEFT JOIN transfer_runtime r ON r.transfer_id=t.id
               WHERE {predicate}
               ORDER BY COALESCE(r.updated_at,m.created_at,0) DESC,t.rowid DESC{limit_clause}"#
        );
        let connection = self.reader();
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([], transfer_job_from_row)?;
        let mut transfers = Vec::new();
        for row in rows {
            transfers.push(row?);
        }
        Ok(transfers)
    }

    pub fn queue_summary(&self, cache_bytes: i64) -> Result<QueueSummary, RepositoryError> {
        let jobs = self.list_active_transfers()?;
        let completed: i64 = self.reader().query_row(
            "SELECT COUNT(*) FROM transfers WHERE status IN ('completed','duplicate')",
            [],
            |row| row.get(0),
        )?;
        let settings = self.settings()?;
        Ok(queue_summary_from_active_jobs(
            &jobs,
            completed.max(0) as usize,
            cache_bytes,
            settings.cache_limit_bytes,
        ))
    }

    #[cfg(test)]
    pub(crate) fn queue_summary_from_jobs(
        &self,
        all_jobs: &[TransferJob],
        cache_bytes: i64,
    ) -> Result<QueueSummary, RepositoryError> {
        let completed = all_jobs
            .iter()
            .filter(|job| matches!(job.status.as_str(), "completed" | "duplicate"))
            .count();
        let jobs = all_jobs
            .iter()
            .filter(|job| !matches!(job.status.as_str(), "completed" | "duplicate" | "cancelled"))
            .cloned()
            .collect::<Vec<_>>();
        let settings = self.settings()?;
        Ok(queue_summary_from_active_jobs(
            &jobs,
            completed,
            cache_bytes,
            settings.cache_limit_bytes,
        ))
    }

    pub fn settings(&self) -> Result<AppSettings, RepositoryError> {
        let connection = self.reader();
        let mut statement = connection.prepare("SELECT key,value FROM app_settings")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut values = HashMap::with_capacity(9);
        for row in rows {
            let (key, value) = row?;
            values.insert(key, value);
        }
        let defaults = AppSettings::default();
        let get = |key: &str| values.get(key).map(String::as_str);
        let prep = get("preparation_concurrency")
            .and_then(|v| v.parse().ok())
            .unwrap_or(defaults.preparation_concurrency)
            .clamp(1, 8);
        let upload = get("upload_concurrency")
            .and_then(|v| v.parse().ok())
            .unwrap_or(defaults.upload_concurrency)
            .clamp(1, 16);
        let download = get("download_concurrency")
            .and_then(|v| v.parse().ok())
            .unwrap_or(defaults.download_concurrency)
            .clamp(1, 8);
        let cache = get("cache_limit_bytes")
            .and_then(|v| v.parse().ok())
            .unwrap_or(defaults.cache_limit_bytes)
            .clamp(256 * 1024 * 1024, 20 * 1024 * 1024 * 1024_i64);
        let remember = get("remember_session").is_some_and(|v| v == "1");
        let conflict = get("conflict_policy")
            .filter(|v| *v == "skip" || *v == "rename")
            .map(str::to_owned)
            .unwrap_or(defaults.conflict_policy);
        let delete_original_after_upload =
            get("delete_original_after_upload").is_some_and(|v| v == "1");
        let speed_limit_bps = get("speed_limit_bps")
            .filter(|v| !v.is_empty())
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v > 0);
        let resource_profile = get("resource_profile")
            .filter(|v| matches!(*v, "low" | "balanced" | "max"))
            .map(str::to_owned)
            .unwrap_or(defaults.resource_profile);
        Ok(AppSettings {
            preparation_concurrency: prep,
            upload_concurrency: upload,
            download_concurrency: download,
            cache_limit_bytes: cache,
            remember_session: remember,
            conflict_policy: conflict,
            delete_original_after_upload,
            speed_limit_bps,
            resource_profile,
        })
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), RepositoryError> {
        const ALLOWED: &[&str] = &[
            "preparation_concurrency",
            "upload_concurrency",
            "download_concurrency",
            "cache_limit_bytes",
            "remember_session",
            "conflict_policy",
            "delete_original_after_upload",
            "speed_limit_bps",
            "resource_profile",
        ];
        if !ALLOWED.contains(&key) {
            return Err(RepositoryError::Database(
                rusqlite::Error::InvalidParameterName(key.to_string()),
            ));
        }
        self.connection.lock().expect("catalog mutex poisoned").execute(
            "INSERT INTO app_settings(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn set_favorite(&self, id: &str, favorite: bool) -> Result<bool, RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let changed = connection.execute(
            "UPDATE files SET favorite = ?1 WHERE id = ?2",
            params![if favorite { 1 } else { 0 }, id],
        )?;
        Ok(changed > 0)
    }

    pub fn has_hash(&self, sha256: &str) -> Result<bool, RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM transfer_metadata tm JOIN transfers t ON t.id = tm.transfer_id WHERE tm.sha256 = ?1 AND t.direction='upload' AND tm.encrypted=0 AND t.status IN ('waiting','analyzing','copying','ready','queued','uploading','confirming','retry_wait','paused','completed')",
            params![sha256],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn upload_source_cleanup(
        &self,
        id: &str,
        require_opt_in: bool,
    ) -> Result<Option<UploadSourceCleanup>, RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let current = connection
            .query_row(
                "SELECT m.source_path,m.size_bytes,m.sha256
                 FROM transfer_metadata m JOIN transfers t ON t.id=m.transfer_id
                 WHERE t.id=?1 AND t.direction='upload' AND t.status='completed'
                   AND m.remote_message_id IS NOT NULL AND m.remote_message_id<>''
                   AND m.source_path IS NOT NULL AND m.source_path<>'' AND m.source_deleted=0
                   AND m.sha256<>'' AND (?2=0 OR m.delete_source_after_upload=1)",
                params![id, if require_opt_in { 1 } else { 0 }],
                |row| {
                    Ok(UploadSourceCleanup {
                        source_path: row.get(0)?,
                        size_bytes: row.get(1)?,
                        sha256: row.get(2)?,
                    })
                },
            )
            .optional()?;
        if current.is_some() || require_opt_in {
            return Ok(current);
        }
        connection
            .query_row(
                "SELECT source_path,size_bytes,sha256 FROM uploaded_sources
                 WHERE transfer_id=?1 AND source_deleted=0 AND source_path<>'' AND sha256<>''",
                [id],
                |row| {
                    Ok(UploadSourceCleanup {
                        source_path: row.get(0)?,
                        size_bytes: row.get(1)?,
                        sha256: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn upload_source_cleanup_candidates(
        &self,
    ) -> Result<Vec<UploadSourceCleanupCandidate>, RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT transfer_id,source_path,size_bytes
             FROM uploaded_sources
             WHERE source_deleted=0 AND source_path<>'' AND sha256<>''
             ORDER BY created_at DESC, transfer_id DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(UploadSourceCleanupCandidate {
                transfer_id: row.get(0)?,
                source_path: row.get(1)?,
                size_bytes: row.get(2)?,
            })
        })?;
        let mut candidates = Vec::new();
        for row in rows {
            candidates.push(row?);
        }
        Ok(candidates)
    }

    pub fn mark_upload_source_path_deleted(
        &self,
        source_path: &str,
    ) -> Result<(), RepositoryError> {
        let mut connection = self.connection.lock().expect("catalog mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE transfer_metadata SET source_deleted=1,source_delete_error=NULL WHERE source_path=?1",
            [source_path],
        )?;
        transaction.execute(
            "UPDATE uploaded_sources SET source_deleted=1,source_delete_error=NULL WHERE source_path=?1",
            [source_path],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_upload_source_delete_error(
        &self,
        id: &str,
        error: &str,
    ) -> Result<(), RepositoryError> {
        let mut connection = self.connection.lock().expect("catalog mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE transfer_metadata SET source_delete_error=?1 WHERE transfer_id=?2",
            params![error, id],
        )?;
        transaction.execute(
            "UPDATE uploaded_sources SET source_delete_error=?1 WHERE transfer_id=?2",
            params![error, id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    pub fn enqueue_transfer(
        &self,
        id: &str,
        file_name: &str,
        local_path: &str,
        size_bytes: i64,
        sha256: &str,
        encrypted: bool,
        status: &str,
    ) -> Result<(), RepositoryError> {
        self.enqueue_transfer_with_folder(
            id,
            file_name,
            local_path,
            Some(local_path),
            size_bytes,
            sha256,
            encrypted,
            status,
            None,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn enqueue_transfer_with_folder(
        &self,
        id: &str,
        file_name: &str,
        local_path: &str,
        source_path: Option<&str>,
        size_bytes: i64,
        sha256: &str,
        encrypted: bool,
        status: &str,
        folder_id: Option<&str>,
        delete_source_after_upload: bool,
    ) -> Result<(), RepositoryError> {
        let mut connection = self.connection.lock().expect("catalog mutex poisoned");
        let transaction = connection.transaction()?;
        ensure_folder_parent(&transaction, folder_id, None)?;
        transaction.execute(
            "INSERT INTO transfers (id,file_name,direction,progress,status,speed_label) VALUES (?1,?2,'upload',0,?3,'Esperando')",
            params![id, file_name, status],
        )?;
        transaction.execute(
            "INSERT INTO transfer_metadata (transfer_id,local_path,source_path,size_bytes,sha256,encrypted,delete_source_after_upload,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,unixepoch())",
            params![id, local_path, source_path, size_bytes, sha256, if encrypted { 1 } else { 0 }, if delete_source_after_upload { 1 } else { 0 }],
        )?;
        transaction.execute(
            "INSERT INTO transfer_runtime (transfer_id,phase,total_bytes,updated_at) VALUES (?1,?2,?3,unixepoch())",
            params![id, status, size_bytes],
        )?;
        transaction.execute(
            "INSERT INTO transfer_folders(transfer_id,folder_id) VALUES (?1,?2)",
            params![id, folder_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub fn create_upload_placeholder(
        &self,
        id: &str,
        file_name: &str,
        source_path: &str,
        size_bytes: i64,
    ) -> Result<(), RepositoryError> {
        self.enqueue_transfer(id, file_name, source_path, size_bytes, "", false, "waiting")
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_upload_placeholder_in_folder(
        &self,
        id: &str,
        file_name: &str,
        prepared_source_path: &str,
        original_source_path: Option<&str>,
        size_bytes: i64,
        folder_id: Option<&str>,
        delete_source_after_upload: bool,
    ) -> Result<(), RepositoryError> {
        self.enqueue_transfer_with_folder(
            id,
            file_name,
            prepared_source_path,
            original_source_path,
            size_bytes,
            "",
            false,
            "waiting",
            folder_id,
            delete_source_after_upload,
        )
    }

    pub fn preparation_source(
        &self,
        id: &str,
    ) -> Result<Option<(String, String, i64)>, RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        connection
            .query_row(
                "SELECT t.file_name,m.local_path,m.size_bytes FROM transfers t JOIN transfer_metadata m ON m.transfer_id=t.id WHERE t.id=?1 AND t.direction='upload' AND m.sha256=''",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn reset_preparation(&self, id: &str) -> Result<(), RepositoryError> {
        self.clear_live_runtime(id);
        let mut connection = self.connection.lock().expect("catalog mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE transfers SET status='waiting',progress=0,speed_label='Esperando preparación' WHERE id=?1 AND direction='upload'",
            [id],
        )?;
        transaction.execute(
            "UPDATE transfer_metadata SET error=NULL WHERE transfer_id=?1",
            [id],
        )?;
        transaction.execute(
            "UPDATE transfer_runtime SET phase='waiting',processed_bytes=0,speed_bps=0,eta_seconds=NULL,pause_requested=0,cancel_requested=0,next_retry_at=NULL,updated_at=unixepoch() WHERE transfer_id=?1",
            [id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn finish_preparation(
        &self,
        id: &str,
        local_path: &str,
        sha256: &str,
        total_bytes: i64,
        duplicate: bool,
    ) -> Result<bool, RepositoryError> {
        let mut connection = self.connection.lock().expect("catalog mutex poisoned");
        let transaction = connection.transaction()?;
        // Concurrent workers may both pass the initial hash check. Resolve the
        // duplicate while committing, before either ready job can be claimed.
        let duplicate = duplicate || transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM transfer_metadata m JOIN transfers t ON t.id=m.transfer_id WHERE m.sha256=?1 AND t.id<>?2 AND t.direction='upload' AND m.encrypted=0 AND t.status IN ('waiting','analyzing','copying','ready','queued','uploading','confirming','retry_wait','paused','completed'))",
            params![sha256, id], |row| row.get::<_, bool>(0),
        )?;
        transaction.execute(
            "UPDATE transfer_metadata SET local_path=?1,sha256=?2,size_bytes=?3,error=NULL WHERE transfer_id=?4",
            params![local_path, sha256, total_bytes, id],
        )?;
        let status = if duplicate { "duplicate" } else { "ready" };
        let label = if duplicate {
            "Duplicado omitido"
        } else {
            "Listo para subir"
        };
        transaction.execute("UPDATE transfers SET status=?1,progress=CASE WHEN ?1='duplicate' THEN 100 ELSE 0 END,speed_label=?2 WHERE id=?3", params![status,label,id])?;
        let prepared_bytes = if duplicate { total_bytes } else { 0 };
        transaction.execute("UPDATE transfer_runtime SET phase=?1,processed_bytes=?2,total_bytes=?3,speed_bps=0,eta_seconds=NULL,pause_requested=0,cancel_requested=0,updated_at=unixepoch(),completed_at=CASE WHEN ?1='duplicate' THEN unixepoch() ELSE NULL END WHERE transfer_id=?4", params![status,prepared_bytes,total_bytes,id])?;
        transaction.commit()?;
        self.clear_live_runtime(id);
        if duplicate {
            self.verified_staging
                .lock()
                .expect("verified staging mutex poisoned")
                .remove(id);
        } else {
            self.remember_verified_staging(id, local_path, total_bytes);
        }
        Ok(duplicate)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_runtime(
        &self,
        id: &str,
        status: &str,
        phase: &str,
        processed_bytes: i64,
        total_bytes: i64,
        speed_bps: i64,
        eta_seconds: Option<i64>,
        speed_label: &str,
        error: Option<&str>,
    ) -> Result<(), RepositoryError> {
        let processed_bytes = processed_bytes.max(0);
        let total_bytes = total_bytes.max(0);
        let speed_bps = speed_bps.max(0);
        let progress = if total_bytes > 0 {
            (processed_bytes.saturating_mul(100) / total_bytes).clamp(0, 100) as u8
        } else {
            0
        };
        let now = Instant::now();

        // UI telemetry can arrive several times per second. Keep the freshest value in
        // memory, but persist only once per second while a phase is stable. Phase/status
        // transitions, errors and completion boundaries are durable immediately.
        let should_persist = {
            let mut live = self
                .live_runtime
                .lock()
                .expect("live transfer runtime mutex poisoned");
            let previous = live.get(id);
            let phase_changed =
                previous.is_none_or(|value| value.phase != phase || value.status != status);
            let crossed_completion = total_bytes > 0
                && processed_bytes >= total_bytes
                && previous.is_none_or(|value| value.processed_bytes < total_bytes);
            let persistence_due = previous.is_none_or(|value| {
                now.duration_since(value.last_persisted) >= Duration::from_secs(1)
            });
            let terminal = matches!(
                status,
                "completed" | "failed" | "paused" | "cancelled" | "duplicate" | "retry_wait"
            );
            let should_persist = phase_changed
                || crossed_completion
                || persistence_due
                || terminal
                || error.is_some();
            let last_persisted = if should_persist {
                now
            } else {
                previous.expect("checked above").last_persisted
            };
            live.insert(
                id.to_string(),
                LiveTransferRuntime {
                    status: status.to_string(),
                    phase: phase.to_string(),
                    progress,
                    processed_bytes,
                    total_bytes,
                    speed_bps,
                    eta_seconds,
                    speed_label: speed_label.to_string(),
                    error: error.map(str::to_string),
                    last_persisted,
                },
            );
            should_persist
        };

        if !should_persist {
            return Ok(());
        }

        let persisted = (|| {
            let mut connection = self.connection.lock().expect("catalog mutex poisoned");
            let transaction = connection.transaction()?;
            transaction.execute(
                "UPDATE transfers SET status=?1,progress=?2,speed_label=?3 WHERE id=?4",
                params![status, progress as i64, speed_label, id],
            )?;
            transaction.execute(
                "UPDATE transfer_metadata SET error=?1 WHERE transfer_id=?2",
                params![error, id],
            )?;
            transaction.execute(
                "UPDATE transfer_runtime SET phase=?1,processed_bytes=?2,total_bytes=?3,speed_bps=?4,eta_seconds=?5,started_at=COALESCE(started_at,unixepoch()),updated_at=unixepoch(),completed_at=CASE WHEN ?6='completed' THEN unixepoch() ELSE completed_at END WHERE transfer_id=?7",
                params![phase,processed_bytes,total_bytes,speed_bps,eta_seconds,status,id],
            )?;
            transaction.commit()?;
            Ok::<(), RepositoryError>(())
        })();

        if persisted.is_err() {
            if let Some(runtime) = self
                .live_runtime
                .lock()
                .expect("live transfer runtime mutex poisoned")
                .get_mut(id)
            {
                runtime.last_persisted = Instant::now() - Duration::from_secs(1);
            }
        }
        persisted
    }

    pub fn update_transfer_state(
        &self,
        id: &str,
        status: &str,
        progress: u8,
        speed_label: &str,
        remote_message_id: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), RepositoryError> {
        let mut connection = self.connection.lock().expect("catalog mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE transfers SET status=?1,progress=?2,speed_label=?3 WHERE id=?4",
            params![status, progress as i64, speed_label, id],
        )?;
        transaction.execute("UPDATE transfer_metadata SET remote_message_id=COALESCE(?1,remote_message_id),error=?2 WHERE transfer_id=?3", params![remote_message_id,error,id])?;
        transaction.execute("UPDATE transfer_runtime SET phase=?1,speed_bps=CASE WHEN ?1 IN ('completed','failed','paused','cancelled','duplicate') THEN 0 ELSE speed_bps END,eta_seconds=CASE WHEN ?1 IN ('completed','failed','paused','cancelled','duplicate') THEN NULL ELSE eta_seconds END,updated_at=unixepoch(),completed_at=CASE WHEN ?1='completed' THEN unixepoch() ELSE completed_at END WHERE transfer_id=?2", params![status,id])?;
        if status == "completed" {
            transaction.execute(
                "INSERT INTO uploaded_sources(
                    transfer_id,file_name,source_path,size_bytes,sha256,remote_message_id,
                    source_deleted,source_delete_error,created_at
                 )
                 SELECT t.id,t.file_name,m.source_path,m.size_bytes,m.sha256,m.remote_message_id,
                        COALESCE(m.source_deleted,0),m.source_delete_error,m.created_at
                 FROM transfers t JOIN transfer_metadata m ON m.transfer_id=t.id
                 WHERE t.id=?1 AND t.direction='upload'
                   AND m.remote_message_id IS NOT NULL AND m.remote_message_id<>''
                   AND m.source_path IS NOT NULL AND m.source_path<>'' AND m.sha256<>''
                 ON CONFLICT(transfer_id) DO UPDATE SET
                    file_name=excluded.file_name,
                    source_path=excluded.source_path,
                    size_bytes=excluded.size_bytes,
                    sha256=excluded.sha256,
                    remote_message_id=excluded.remote_message_id,
                    source_deleted=MAX(uploaded_sources.source_deleted,excluded.source_deleted),
                    source_delete_error=CASE WHEN uploaded_sources.source_deleted=1 THEN NULL ELSE excluded.source_delete_error END",
                [id],
            )?;
        }
        transaction.commit()?;
        self.clear_live_runtime(id);
        Ok(())
    }

    pub fn set_td_file_id(&self, id: &str, file_id: i32) -> Result<(), RepositoryError> {
        self.connection
            .lock()
            .expect("catalog mutex poisoned")
            .execute(
            "UPDATE transfer_runtime SET td_file_id=?1,updated_at=unixepoch() WHERE transfer_id=?2",
            params![file_id, id],
        )?;
        Ok(())
    }

    pub fn transfer_control(&self, id: &str) -> Result<TransferControl, RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        connection.query_row(
            "SELECT pause_requested,cancel_requested FROM transfer_runtime WHERE transfer_id=?1",
            [id],
            |row| Ok(TransferControl {
                pause_requested: row.get::<_,i64>(0)? != 0,
                cancel_requested: row.get::<_,i64>(1)? != 0,
            }),
        ).map_err(Into::into)
    }

    pub fn request_pause(&self, id: &str) -> Result<(), RepositoryError> {
        self.connection.lock().expect("catalog mutex poisoned").execute(
            "UPDATE transfer_runtime SET pause_requested=1,updated_at=unixepoch() WHERE transfer_id=?1",
            [id],
        )?;
        Ok(())
    }

    pub fn request_cancel(&self, id: &str) -> Result<(), RepositoryError> {
        self.connection.lock().expect("catalog mutex poisoned").execute(
            "UPDATE transfer_runtime SET cancel_requested=1,updated_at=unixepoch() WHERE transfer_id=?1",
            [id],
        )?;
        Ok(())
    }

    pub fn resume(&self, id: &str) -> Result<(), RepositoryError> {
        self.clear_live_runtime(id);
        let mut connection = self.connection.lock().expect("catalog mutex poisoned");
        let transaction = connection.transaction()?;
        let direction: String =
            transaction.query_row("SELECT direction FROM transfers WHERE id=?1", [id], |r| {
                r.get(0)
            })?;
        let next = if direction == "upload" {
            "ready"
        } else {
            "queued"
        };
        let changed = transaction.execute("UPDATE transfers SET status=?1,speed_label='En cola',progress=CASE WHEN ?1='ready' THEN 0 ELSE progress END WHERE id=?2 AND status IN ('paused','failed','cancelled','retry_wait')", params![next,id])?;
        if changed == 0 {
            return Ok(());
        }
        transaction.execute("UPDATE transfer_runtime SET phase=?1,pause_requested=0,cancel_requested=0,next_retry_at=NULL,speed_bps=0,eta_seconds=NULL,attempts=0,updated_at=unixepoch() WHERE transfer_id=?2", params![next,id])?;
        transaction.execute(
            "UPDATE transfer_metadata SET error=NULL WHERE transfer_id=?1",
            [id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_cancelled(&self, id: &str) -> Result<(), RepositoryError> {
        self.update_transfer_state(id, "cancelled", 0, "Cancelada", None, None)?;
        self.connection
            .lock()
            .expect("catalog mutex poisoned")
            .execute(
            "UPDATE transfer_runtime SET cancel_requested=0,pause_requested=0 WHERE transfer_id=?1",
            [id],
        )?;
        Ok(())
    }

    pub fn mark_paused(&self, id: &str) -> Result<(), RepositoryError> {
        self.update_transfer_state(id, "paused", 0, "Pausada", None, None)?;
        self.connection
            .lock()
            .expect("catalog mutex poisoned")
            .execute(
                "UPDATE transfer_runtime SET pause_requested=0 WHERE transfer_id=?1",
                [id],
            )?;
        Ok(())
    }

    pub fn mark_retry_or_failed(&self, id: &str, error: &str) -> Result<bool, RepositoryError> {
        self.clear_live_runtime(id);
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let (attempts, max_attempts): (i32, i32) = connection.query_row(
            "SELECT attempts,max_attempts FROM transfer_runtime WHERE transfer_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let next_attempt = attempts + 1;
        if next_attempt < max_attempts {
            let delay =
                (5_i64 * 2_i64.pow((next_attempt.saturating_sub(1) as u32).min(6))).min(300);
            connection.execute(
                "UPDATE transfers SET status='retry_wait',speed_label=?1 WHERE id=?2",
                params![format!("Reintento automático en {delay} s"), id],
            )?;
            connection.execute(
                "UPDATE transfer_metadata SET error=?1 WHERE transfer_id=?2",
                params![error, id],
            )?;
            connection.execute("UPDATE transfer_runtime SET phase='retry_wait',attempts=?1,next_retry_at=unixepoch()+?2,speed_bps=0,eta_seconds=?2,updated_at=unixepoch() WHERE transfer_id=?3",params![next_attempt,delay,id])?;
            Ok(true)
        } else {
            connection.execute(
                "UPDATE transfers SET status='failed',speed_label='Error' WHERE id=?1",
                [id],
            )?;
            connection.execute(
                "UPDATE transfer_metadata SET error=?1 WHERE transfer_id=?2",
                params![error, id],
            )?;
            connection.execute("UPDATE transfer_runtime SET phase='error',attempts=?1,next_retry_at=NULL,speed_bps=0,eta_seconds=NULL,updated_at=unixepoch() WHERE transfer_id=?2",params![next_attempt,id])?;
            Ok(false)
        }
    }

    pub fn release_due_retries(&self) -> Result<(), RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        connection.execute(
            "UPDATE transfers SET status=CASE direction WHEN 'upload' THEN 'ready' ELSE 'queued' END,speed_label='Reintentando' WHERE id IN (SELECT transfer_id FROM transfer_runtime WHERE next_retry_at IS NOT NULL AND next_retry_at<=unixepoch()) AND status='retry_wait'",
            [],
        )?;
        connection.execute("UPDATE transfer_runtime SET phase=CASE (SELECT direction FROM transfers WHERE id=transfer_id) WHEN 'upload' THEN 'ready' ELSE 'queued' END,next_retry_at=NULL,updated_at=unixepoch() WHERE next_retry_at IS NOT NULL AND next_retry_at<=unixepoch()",[])?;
        Ok(())
    }

    pub fn next_retry_delay(&self) -> Result<Option<Duration>, RepositoryError> {
        let connection = self.reader();
        let seconds: Option<i64> = connection.query_row(
            "SELECT CASE
                 WHEN MIN(next_retry_at) IS NULL THEN NULL
                 ELSE MAX(MIN(next_retry_at)-unixepoch(),0)
             END
             FROM transfer_runtime
             WHERE next_retry_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(seconds.map(|value| Duration::from_secs(value.max(0) as u64)))
    }

    pub fn active_count(&self) -> Result<usize, RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let count:i64=connection.query_row("SELECT COUNT(*) FROM transfers WHERE status IN ('analyzing','copying','uploading','downloading','confirming','running')",[],|r|r.get(0))?;
        Ok(count.max(0) as usize)
    }

    pub fn upload_staging_paths_in_use(&self) -> Result<Vec<String>, RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT m.local_path
             FROM transfer_metadata m
             JOIN transfers t ON t.id=m.transfer_id
             WHERE t.direction='upload'
               AND t.status NOT IN ('completed','duplicate')
               AND m.local_path<>''",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut paths = Vec::new();
        for row in rows {
            paths.push(row?);
        }
        Ok(paths)
    }

    #[allow(dead_code)]
    pub fn unfinished_count(&self) -> Result<usize, RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM transfers WHERE status NOT IN ('completed','duplicate','cancelled')",
            [],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as usize)
    }

    pub fn clear_transfer_history(&self) -> Result<usize, RepositoryError> {
        let mut connection = self.connection.lock().expect("catalog mutex poisoned");
        let transaction = connection.transaction()?;
        // Preserve the verified local-source metadata before transfer rows cascade away.
        // This defensive backfill keeps manual cleanup working even if an older or
        // interrupted completion path failed to populate uploaded_sources earlier.
        transaction.execute(
            "INSERT INTO uploaded_sources(
                transfer_id,file_name,source_path,size_bytes,sha256,remote_message_id,
                source_deleted,source_delete_error,created_at
             )
             SELECT t.id,t.file_name,m.source_path,m.size_bytes,m.sha256,m.remote_message_id,
                    COALESCE(m.source_deleted,0),m.source_delete_error,m.created_at
             FROM transfers t
             JOIN transfer_metadata m ON m.transfer_id=t.id
             WHERE t.direction='upload' AND t.status='completed'
               AND m.remote_message_id IS NOT NULL AND m.remote_message_id<>''
               AND m.source_path IS NOT NULL AND m.source_path<>'' AND m.sha256<>''
             ON CONFLICT(transfer_id) DO UPDATE SET
                file_name=excluded.file_name,
                source_path=excluded.source_path,
                size_bytes=excluded.size_bytes,
                sha256=excluded.sha256,
                remote_message_id=excluded.remote_message_id,
                source_deleted=MAX(uploaded_sources.source_deleted,excluded.source_deleted),
                source_delete_error=CASE WHEN uploaded_sources.source_deleted=1 THEN NULL ELSE excluded.source_delete_error END",
            [],
        )?;
        let changed = transaction.execute(
            "DELETE FROM transfers WHERE status IN ('completed','duplicate','cancelled')",
            [],
        )?;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn pause_all_active(&self) -> Result<(), RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        connection.execute("UPDATE transfer_runtime SET pause_requested=1,updated_at=unixepoch() WHERE transfer_id IN (SELECT id FROM transfers WHERE status IN ('analyzing','copying','uploading','downloading','confirming','running'))",[])?;
        Ok(())
    }
}

fn queue_summary_from_active_jobs(
    jobs: &[TransferJob],
    completed: usize,
    cache_bytes: i64,
    cache_limit_bytes: i64,
) -> QueueSummary {
    let total = jobs.len();
    let failed = jobs.iter().filter(|job| job.status == "failed").count();
    let active = jobs
        .iter()
        .filter(|job| {
            matches!(
                job.status.as_str(),
                "analyzing" | "copying" | "uploading" | "downloading" | "confirming" | "running"
            )
        })
        .count();
    let pending = jobs
        .iter()
        .filter(|job| {
            !matches!(
                job.status.as_str(),
                "completed" | "duplicate" | "failed" | "cancelled"
            )
        })
        .count();
    let total_bytes: i64 = jobs.iter().map(|job| job.total_bytes.max(0)).sum();
    let processed_bytes: i64 = jobs
        .iter()
        .map(|job| job.processed_bytes.clamp(0, job.total_bytes.max(0)))
        .sum();
    let speed_bps: i64 = jobs
        .iter()
        .filter(|job| {
            matches!(
                job.status.as_str(),
                "uploading" | "downloading" | "copying" | "analyzing" | "running"
            )
        })
        .map(|job| job.speed_bps.max(0))
        .sum();
    let remaining = (total_bytes - processed_bytes).max(0);
    let eta_seconds = if speed_bps > 0 {
        Some((remaining + speed_bps - 1) / speed_bps)
    } else {
        None
    };
    QueueSummary {
        total,
        completed,
        pending,
        failed,
        active,
        processed_bytes,
        total_bytes,
        speed_bps,
        eta_seconds,
        cache_bytes,
        cache_limit_bytes,
    }
}

fn transfer_job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TransferJob> {
    let raw_progress: i64 = row.get(3)?;
    let direction: String = row.get(2)?;
    let status: String = row.get(4)?;
    let source_path: Option<String> = row.get(14)?;
    let source_deleted = row.get::<_, i64>(15)? != 0;
    let remote_message_id: Option<String> = row.get(17)?;
    let active = matches!(
        status.as_str(),
        "waiting"
            | "analyzing"
            | "copying"
            | "ready"
            | "queued"
            | "uploading"
            | "downloading"
            | "confirming"
            | "retry_wait"
            | "running"
    );
    let source_delete_available = direction == "upload"
        && status == "completed"
        && !source_deleted
        && source_path.as_deref().is_some_and(|path| !path.is_empty())
        && remote_message_id
            .as_deref()
            .is_some_and(|id| !id.is_empty());
    Ok(TransferJob {
        id: row.get(0)?,
        file_name: row.get(1)?,
        direction,
        progress: raw_progress.clamp(0, 100) as u8,
        status: status.clone(),
        speed_label: row.get(5)?,
        phase: row.get(6)?,
        processed_bytes: row.get(7)?,
        total_bytes: row.get(8)?,
        speed_bps: row.get(9)?,
        eta_seconds: row.get(10)?,
        attempts: row.get(11)?,
        max_attempts: row.get(12)?,
        error: row.get(13)?,
        can_pause: active && status != "retry_wait",
        can_retry: matches!(status.as_str(), "failed" | "paused" | "cancelled"),
        can_cancel: !matches!(status.as_str(), "completed" | "duplicate" | "cancelled"),
        source_delete_available,
        source_deleted,
        source_delete_error: row.get(16)?,
        started_at: row.get(18)?,
        updated_at: row.get(19)?,
    })
}

fn initialize_catalog_fts(connection: &Connection) -> Result<bool, RepositoryError> {
    let schema = connection.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS files_fts
        USING fts5(id UNINDEXED,name,extension,tags,tokenize='trigram');

        CREATE TRIGGER IF NOT EXISTS trg_files_fts_insert
        AFTER INSERT ON files BEGIN
            INSERT INTO files_fts(id,name,extension,tags)
            VALUES (NEW.id,NEW.name,NEW.extension,NEW.tags_json);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_files_fts_delete
        AFTER DELETE ON files BEGIN
            DELETE FROM files_fts WHERE id=OLD.id;
        END;
        CREATE TRIGGER IF NOT EXISTS trg_files_fts_update
        AFTER UPDATE OF name,extension,tags_json ON files BEGIN
            DELETE FROM files_fts WHERE id=OLD.id;
            INSERT INTO files_fts(id,name,extension,tags)
            VALUES (NEW.id,NEW.name,NEW.extension,NEW.tags_json);
        END;
        "#,
    );
    if schema.is_err() {
        let _ = connection.execute(
            "INSERT INTO app_meta(key,value) VALUES ('catalog_fts5','0')
             ON CONFLICT(key) DO UPDATE SET value='0'",
            [],
        );
        return Ok(false);
    }

    let file_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
    let fts_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM files_fts", [], |row| row.get(0))?;
    if file_count != fts_count {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute("DELETE FROM files_fts", [])?;
        transaction.execute(
            "INSERT INTO files_fts(id,name,extension,tags)
             SELECT id,name,extension,tags_json FROM files",
            [],
        )?;
        transaction.commit()?;
    }
    connection.execute(
        "INSERT INTO app_meta(key,value) VALUES ('catalog_fts5','1')
         ON CONFLICT(key) DO UPDATE SET value='1'",
        [],
    )?;
    Ok(true)
}

fn catalog_fts_available(connection: &Connection) -> bool {
    connection
        .query_row(
            "SELECT value='1' FROM app_meta WHERE key='catalog_fts5'",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap_or(false)
}

fn natural_name_compare(left: &str, right: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering as Cmp;

    let left_bytes = left.as_bytes();
    let right_bytes = right.as_bytes();
    let mut li = 0usize;
    let mut ri = 0usize;

    while li < left_bytes.len() && ri < right_bytes.len() {
        if left_bytes[li].is_ascii_digit() && right_bytes[ri].is_ascii_digit() {
            let left_start = li;
            while li < left_bytes.len() && left_bytes[li].is_ascii_digit() {
                li += 1;
            }
            let right_start = ri;
            while ri < right_bytes.len() && right_bytes[ri].is_ascii_digit() {
                ri += 1;
            }

            let left_run = &left[left_start..li];
            let right_run = &right[right_start..ri];
            let left_sig = left_run.trim_start_matches('0');
            let right_sig = right_run.trim_start_matches('0');
            let left_sig = if left_sig.is_empty() { "0" } else { left_sig };
            let right_sig = if right_sig.is_empty() { "0" } else { right_sig };

            match left_sig.len().cmp(&right_sig.len()) {
                Cmp::Equal => {}
                other => return other,
            }
            match left_sig.cmp(right_sig) {
                Cmp::Equal => {}
                other => return other,
            }
            match left_run.len().cmp(&right_run.len()) {
                Cmp::Equal => {}
                other => return other,
            }
            continue;
        }

        let left_char = left[li..].chars().next().expect("valid utf-8");
        let right_char = right[ri..].chars().next().expect("valid utf-8");

        let mut left_fold = left_char.to_lowercase();
        let mut right_fold = right_char.to_lowercase();
        loop {
            match (left_fold.next(), right_fold.next()) {
                (Some(a), Some(b)) if a == b => {}
                (Some(a), Some(b)) => return a.cmp(&b),
                (None, None) => break,
                (None, Some(_)) => return Cmp::Less,
                (Some(_), None) => return Cmp::Greater,
            }
        }

        li += left_char.len_utf8();
        ri += right_char.len_utf8();
    }

    left_bytes.len().cmp(&right_bytes.len())
}

fn configure_connection(connection: &Connection, read_only: bool) -> Result<(), RepositoryError> {
    connection.create_collation("NUVIO_NATURAL", natural_name_compare)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    // The catalog is reconstructible from Telegram. WAL + NORMAL lowers fsync cost
    // while keeping transactional crash consistency. Readers get their own page cache
    // so long catalog scans don't hold the writer mutex or evict writer statements.
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    connection.pragma_update(None, "cache_size", -65_536_i64)?;
    connection.pragma_update(None, "mmap_size", 268_435_456_i64)?;
    connection.pragma_update(None, "wal_autocheckpoint", 8_192_i64)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    if read_only {
        connection.pragma_update(None, "query_only", "ON")?;
    }
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

pub(crate) fn validate_folder_name(name: &str) -> Result<(), RepositoryError> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 120
        || matches!(name, "." | "..")
        || name
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
    {
        return Err(RepositoryError::Database(
            rusqlite::Error::InvalidParameterName("Nombre de carpeta inválido".to_string()),
        ));
    }
    Ok(())
}

pub(crate) fn ensure_folder_parent(
    connection: &Connection,
    parent_id: Option<&str>,
    moving_folder_id: Option<&str>,
) -> Result<(), RepositoryError> {
    let Some(parent_id) = parent_id else {
        return Ok(());
    };
    if moving_folder_id == Some(parent_id) {
        return Err(RepositoryError::Database(
            rusqlite::Error::InvalidParameterName(
                "Una carpeta no puede contenerse a sí misma".to_string(),
            ),
        ));
    }
    let active: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM folders WHERE id=?1 AND trashed=0)",
        [parent_id],
        |row| row.get(0),
    )?;
    if !active {
        return Err(RepositoryError::Database(
            rusqlite::Error::QueryReturnedNoRows,
        ));
    }
    if let Some(folder_id) = moving_folder_id {
        let creates_cycle: bool = connection.query_row(
            "WITH RECURSIVE ancestors(id,parent_id) AS (
                 SELECT id,parent_id FROM folders WHERE id=?1
                 UNION
                 SELECT f.id,f.parent_id FROM folders f JOIN ancestors a ON f.id=a.parent_id
             )
             SELECT EXISTS(SELECT 1 FROM ancestors WHERE id=?2)",
            params![parent_id, folder_id],
            |row| row.get(0),
        )?;
        if creates_cycle {
            return Err(RepositoryError::Database(
                rusqlite::Error::InvalidParameterName(
                    "No puedes mover una carpeta dentro de una de sus subcarpetas".to_string(),
                ),
            ));
        }
    }
    Ok(())
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), RepositoryError> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for existing in columns {
        if existing? == column {
            return Ok(());
        }
    }
    connection.execute(&format!("ALTER TABLE {table} ADD COLUMN {definition}"), [])?;
    Ok(())
}

fn ensure_unique_folder_name(
    connection: &Connection,
    name: &str,
    parent_id: Option<&str>,
    except_id: Option<&str>,
) -> Result<(), RepositoryError> {
    let duplicate: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM folders
             WHERE lower(name)=lower(?1)
               AND ((parent_id=?2) OR (parent_id IS NULL AND ?2 IS NULL))
               AND trashed=0
               AND (?3 IS NULL OR id<>?3)
         )",
        params![name.trim(), parent_id, except_id],
        |row| row.get(0),
    )?;
    if duplicate {
        return Err(RepositoryError::Database(
            rusqlite::Error::InvalidParameterName(
                "Ya existe una carpeta con ese nombre en esta ubicación".to_string(),
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_upload_source_cleanup_is_opt_in_and_persistent() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        assert!(!repo.settings().unwrap().delete_original_after_upload);
        repo.set_setting("delete_original_after_upload", "1")
            .unwrap();
        assert!(repo.settings().unwrap().delete_original_after_upload);

        repo.create_upload_placeholder_in_folder(
            "cleanup",
            "photo.jpg",
            "prepared.jpg",
            Some("original.jpg"),
            3,
            None,
            true,
        )
        .unwrap();
        repo.finish_preparation("cleanup", "prepared.jpg", "abc", 3, false)
            .unwrap();
        repo.update_transfer_state(
            "cleanup",
            "completed",
            100,
            "Guardado en Telegram",
            Some("42"),
            None,
        )
        .unwrap();
        let candidate = repo
            .upload_source_cleanup("cleanup", true)
            .unwrap()
            .unwrap();
        assert_eq!(candidate.source_path, "original.jpg");
        assert_eq!(candidate.size_bytes, 3);
        assert_eq!(candidate.sha256, "abc");
        repo.mark_upload_source_path_deleted("original.jpg")
            .unwrap();
        assert!(repo
            .upload_source_cleanup("cleanup", false)
            .unwrap()
            .is_none());
        let transfer = repo.list_transfers().unwrap().remove(0);
        assert!(transfer.source_deleted);
        assert!(!transfer.source_delete_available);
    }

    #[test]
    fn source_path_cleanup_marks_every_ledger_row_for_the_removed_file() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        for (id, hash, message) in [("one", "hash-one", "41"), ("two", "hash-two", "42")] {
            repo.create_upload_placeholder_in_folder(
                id,
                &format!("{id}.bin"),
                &format!("prepared-{id}.bin"),
                Some("same-original.bin"),
                10,
                None,
                false,
            )
            .unwrap();
            repo.finish_preparation(id, &format!("prepared-{id}.bin"), hash, 10, false)
                .unwrap();
            repo.update_transfer_state(
                id,
                "completed",
                100,
                "Guardado en Telegram",
                Some(message),
                None,
            )
            .unwrap();
        }
        assert_eq!(repo.upload_source_cleanup_candidates().unwrap().len(), 2);
        repo.mark_upload_source_path_deleted("same-original.bin")
            .unwrap();
        assert!(repo.upload_source_cleanup_candidates().unwrap().is_empty());
        assert!(repo
            .list_transfers()
            .unwrap()
            .into_iter()
            .all(|job| job.source_deleted));
    }

    #[test]
    fn cancelled_upload_keeps_staging_path_because_it_is_resumable() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.create_upload_placeholder_in_folder(
            "cancelled",
            "part.zip",
            "staging/cancelled/part.zip",
            None,
            100,
            None,
            false,
        )
        .unwrap();
        repo.finish_preparation(
            "cancelled",
            "staging/cancelled/part.zip",
            "hash-cancelled",
            100,
            false,
        )
        .unwrap();
        repo.mark_cancelled("cancelled").unwrap();
        assert_eq!(
            repo.upload_staging_paths_in_use().unwrap(),
            vec!["staging/cancelled/part.zip".to_string()]
        );
    }

    #[test]
    fn manual_cleanup_candidates_ignore_auto_delete_opt_in() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.create_upload_placeholder_in_folder(
            "manual-cleanup",
            "photo.jpg",
            "prepared.jpg",
            Some("original.jpg"),
            99,
            None,
            false,
        )
        .unwrap();
        repo.finish_preparation("manual-cleanup", "prepared.jpg", "abc", 99, false)
            .unwrap();
        repo.update_transfer_state(
            "manual-cleanup",
            "completed",
            100,
            "Guardado en Telegram",
            Some("99"),
            None,
        )
        .unwrap();
        assert!(repo
            .upload_source_cleanup("manual-cleanup", true)
            .unwrap()
            .is_none());
        let candidates = repo.upload_source_cleanup_candidates().unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].transfer_id, "manual-cleanup");
        assert_eq!(candidates[0].source_path, "original.jpg");
        assert_eq!(candidates[0].size_bytes, 99);

        // Simulate an older/interrupted completion path that did not persist the
        // durable cleanup ledger. Clearing history must backfill it first.
        {
            let connection = repo.connection.lock().unwrap();
            connection
                .execute(
                    "DELETE FROM uploaded_sources WHERE transfer_id='manual-cleanup'",
                    [],
                )
                .unwrap();
        }
        assert!(repo.upload_source_cleanup_candidates().unwrap().is_empty());

        assert_eq!(repo.clear_transfer_history().unwrap(), 1);
        assert!(repo.list_transfers().unwrap().is_empty());
        let retained = repo.upload_source_cleanup_candidates().unwrap();
        assert_eq!(retained.len(), 1);
        assert!(repo
            .upload_source_cleanup("manual-cleanup", false)
            .unwrap()
            .is_some());
    }

    #[test]
    fn sync_delta_tracks_inserts_updates_and_deletes_after_cursor() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        {
            let connection = repo.connection.lock().unwrap();
            connection.execute(
                "INSERT INTO files(id,name,extension,kind,size_bytes,updated_at) VALUES('old','old','txt','text',1,'2026-01-01T00:00:00Z')",
                [],
            ).unwrap();
        }
        let cursor = repo.catalog_change_cursor().unwrap();
        {
            let connection = repo.connection.lock().unwrap();
            connection.execute(
                "INSERT INTO files(id,name,extension,kind,size_bytes,updated_at) VALUES('new','new','jpg','image',2,'2026-01-02T00:00:00Z')",
                [],
            ).unwrap();
            connection
                .execute("UPDATE files SET favorite=1 WHERE id='old'", [])
                .unwrap();
        }
        let delta = repo.sync_file_delta(cursor).unwrap();
        assert!(delta.cursor > cursor);
        assert_eq!(delta.files.len(), 2);
        assert!(delta.files.iter().any(|file| file.id == "new"));
        assert!(delta
            .files
            .iter()
            .any(|file| file.id == "old" && file.favorite));
        assert!(delta.removed_ids.is_empty());

        {
            let connection = repo.connection.lock().unwrap();
            connection
                .execute("DELETE FROM files WHERE id='new'", [])
                .unwrap();
        }
        let deleted = repo.sync_file_delta(delta.cursor).unwrap();
        assert_eq!(deleted.removed_ids, vec!["new"]);
        assert!(deleted.files.is_empty());
        let empty = repo.sync_file_delta(deleted.cursor).unwrap();
        assert!(empty.files.is_empty());
        assert!(empty.removed_ids.is_empty());
    }

    #[test]
    fn parallel_preparations_only_queue_one_copy_of_the_same_hash() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        for id in ["first", "second"] {
            repo.create_upload_placeholder(id, "file", "source", 10)
                .unwrap();
            assert!(!repo.has_hash("same-content").unwrap());
        }
        std::thread::scope(|scope| {
            for id in ["first", "second"] {
                let repository = &repo;
                scope.spawn(move || {
                    repository
                        .finish_preparation(id, "staged", "same-content", 10, false)
                        .unwrap()
                });
            }
        });
        let jobs = repo.list_transfers().unwrap();
        assert_eq!(jobs.iter().filter(|job| job.status == "ready").count(), 1);
        assert_eq!(
            jobs.iter().filter(|job| job.status == "duplicate").count(),
            1
        );
    }

    #[test]
    fn queue_keeps_all_jobs_beyond_250() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        for index in 0..301 {
            repo.enqueue_transfer(
                &format!("job-{index}"),
                "file",
                "file",
                10,
                "hash",
                false,
                "ready",
            )
            .unwrap();
        }
        let jobs = repo.list_transfers().unwrap();
        assert_eq!(jobs.len(), 301);
        let summary = repo.queue_summary(0).unwrap();
        let snapshot_summary = repo.queue_summary_from_jobs(&jobs, 0).unwrap();
        assert_eq!(summary.pending, 301);
        assert_eq!(summary.total_bytes, 3010);
        assert_eq!(snapshot_summary.pending, summary.pending);
        assert_eq!(snapshot_summary.total, summary.total);
        assert_eq!(snapshot_summary.total_bytes, summary.total_bytes);
        assert_eq!(snapshot_summary.processed_bytes, summary.processed_bytes);
    }

    #[test]
    fn live_progress_is_visible_while_sqlite_checkpoints_are_throttled() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.enqueue_transfer("job", "file", "file", 100, "hash", false, "ready")
            .unwrap();

        repo.update_runtime(
            "job",
            "uploading",
            "uploading",
            10,
            100,
            1_000,
            Some(1),
            "Subiendo",
            None,
        )
        .unwrap();
        let persisted: i64 = repo
            .connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT processed_bytes FROM transfer_runtime WHERE transfer_id='job'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted, 10);

        // Same phase inside the one-second persistence window stays in memory only.
        repo.update_runtime(
            "job",
            "uploading",
            "uploading",
            20,
            100,
            2_000,
            Some(1),
            "Subiendo",
            None,
        )
        .unwrap();
        let persisted: i64 = repo
            .connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT processed_bytes FROM transfer_runtime WHERE transfer_id='job'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted, 10);

        let live = repo.list_active_transfers().unwrap().remove(0);
        assert_eq!(live.processed_bytes, 20);
        assert_eq!(live.speed_bps, 2_000);
        assert_eq!(repo.queue_summary(0).unwrap().processed_bytes, 20);

        // Reaching the end of a phase is a crash-safe checkpoint even inside the window.
        repo.update_runtime(
            "job",
            "uploading",
            "uploading",
            100,
            100,
            0,
            Some(0),
            "Subida completa",
            None,
        )
        .unwrap();
        let persisted: i64 = repo
            .connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT processed_bytes FROM transfer_runtime WHERE transfer_id='job'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted, 100);
    }

    #[test]
    fn next_retry_delay_tracks_the_earliest_scheduled_retry() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.enqueue_transfer("later", "file", "file", 10, "hash-a", false, "ready")
            .unwrap();
        repo.enqueue_transfer("sooner", "file", "file", 10, "hash-b", false, "ready")
            .unwrap();
        {
            let connection = repo.connection.lock().unwrap();
            connection.execute(
                "UPDATE transfer_runtime SET next_retry_at=unixepoch()+60 WHERE transfer_id='later'",
                [],
            ).unwrap();
            connection.execute(
                "UPDATE transfer_runtime SET next_retry_at=unixepoch()+20 WHERE transfer_id='sooner'",
                [],
            ).unwrap();
        }
        let delay = repo.next_retry_delay().unwrap().unwrap().as_secs();
        assert!(
            (15..=20).contains(&delay),
            "unexpected retry delay: {delay}"
        );
    }

    #[test]
    fn resuming_completed_job_does_not_corrupt_phase() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.enqueue_transfer("job", "file", "file", 10, "hash", false, "completed")
            .unwrap();
        repo.resume("job").unwrap();
        let job = repo.list_transfers().unwrap().remove(0);
        assert_eq!(job.status, "completed");
        assert_eq!(job.phase, "completed");
    }

    #[test]
    fn history_cursor_ignores_active_progress_and_tracks_terminal_changes() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.enqueue_transfer("job", "file", "file", 10, "hash", false, "ready")
            .unwrap();
        let before = repo.transfer_history_cursor().unwrap();
        {
            let connection = repo.connection.lock().unwrap();
            connection
                .execute(
                    "UPDATE transfer_runtime SET processed_bytes=5,updated_at=unixepoch() WHERE transfer_id='job'",
                    [],
                )
                .unwrap();
        }
        assert_eq!(repo.transfer_history_cursor().unwrap(), before);
        {
            let connection = repo.connection.lock().unwrap();
            connection
                .execute("UPDATE transfers SET status='completed' WHERE id='job'", [])
                .unwrap();
        }
        let completed = repo.transfer_history_cursor().unwrap();
        assert!(completed > before);
        assert_eq!(repo.list_transfer_history(1000).unwrap().len(), 1);
        assert_eq!(repo.clear_transfer_history().unwrap(), 1);
        assert!(repo.transfer_history_cursor().unwrap() > completed);
    }

    #[test]
    fn terminal_transfers_leave_the_active_queue_but_remain_in_history() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.enqueue_transfer(
            "done",
            "done.txt",
            "done.txt",
            10,
            "hash-a",
            false,
            "completed",
        )
        .unwrap();
        repo.enqueue_transfer(
            "queued",
            "queued.txt",
            "queued.txt",
            20,
            "hash-b",
            false,
            "ready",
        )
        .unwrap();

        let summary = repo.queue_summary(0).unwrap();
        assert_eq!(summary.total, 1);
        assert_eq!(summary.pending, 1);
        assert_eq!(summary.completed, 1);
        assert_eq!(repo.unfinished_count().unwrap(), 1);
        assert_eq!(repo.list_transfers().unwrap().len(), 2);

        assert_eq!(repo.clear_transfer_history().unwrap(), 1);
        let remaining = repo.list_transfers().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "queued");
    }

    #[test]
    fn upload_reserves_folder_before_worker_can_claim_it() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.init_cloud().unwrap();
        repo.create_folder_local("folder-a", "Fotos", None).unwrap();
        repo.create_upload_placeholder_in_folder(
            "upload",
            "photo.jpg",
            "source",
            Some("source"),
            3,
            Some("folder-a"),
            false,
        )
        .unwrap();
        assert!(!repo.folder_is_empty("folder-a").unwrap());
        repo.finish_preparation("upload", "prepared", "hash", 3, false)
            .unwrap();
        let job = repo.claim_pending("upload").unwrap().unwrap();
        assert_eq!(job.folder_id.as_deref(), Some("folder-a"));
        assert!(repo
            .create_upload_placeholder_in_folder(
                "invalid",
                "photo.jpg",
                "source",
                Some("source"),
                3,
                Some("missing"),
                false
            )
            .is_err());
        assert_eq!(repo.list_transfers().unwrap().len(), 1);
    }

    #[test]
    fn folders_support_subfolders_and_reject_cycles() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.create_folder_local("folder-a", "Trabajo", None)
            .unwrap();
        repo.create_folder_local("folder-b", "2026", Some("folder-a"))
            .unwrap();
        let folders = repo.list_folders().unwrap();
        assert_eq!(folders.len(), 2);
        assert_eq!(
            folders
                .iter()
                .find(|folder| folder.id == "folder-b")
                .unwrap()
                .parent_id
                .as_deref(),
            Some("folder-a")
        );
        assert!(repo
            .validate_folder_update("folder-a", "Trabajo", Some("folder-b"))
            .is_err());
        assert!(repo
            .create_folder_local("folder-c", "2026", Some("folder-a"))
            .is_err());
    }

    #[test]
    fn files_can_move_between_virtual_folders_and_root() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.create_folder_local("folder-a", "Fotos", None).unwrap();
        repo.connection.lock().unwrap().execute(
            "INSERT INTO files(id,name,extension,kind,size_bytes,updated_at,folder,provider) VALUES ('file-1','foto','jpg','image',123,'2026-01-01T00:00:00Z','Mi unidad','telegram')",
            [],
        ).unwrap();
        assert_eq!(
            repo.set_file_folder_local(&["file-1".into()], Some("folder-a"))
                .unwrap(),
            1
        );
        let file = repo.list_files().unwrap().remove(0);
        assert_eq!(file.folder_id.as_deref(), Some("folder-a"));
        assert_eq!(file.folder, "Fotos");
        assert_eq!(
            repo.set_file_folder_local(&["file-1".into()], None)
                .unwrap(),
            1
        );
        let file = repo.list_files().unwrap().remove(0);
        assert!(file.folder_id.is_none());
        assert_eq!(file.folder, "Mi unidad");
    }

    #[test]
    #[ignore = "performance budget; run with pnpm qa:perf"]
    fn catalog_performance_budget_250k() {
        fn elapsed_ms(start: Instant) -> u128 {
            start.elapsed().as_millis()
        }

        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("perf.db")).unwrap();
        {
            let mut connection = repo.connection.lock().unwrap();
            let tx = connection.transaction().unwrap();
            {
                let mut folder = tx
                    .prepare(
                        "INSERT INTO folders(id,name,parent_id,trashed,created_at,updated_at)
                         VALUES(?1,?2,NULL,0,1,1)",
                    )
                    .unwrap();
                for index in 0..1000 {
                    folder
                        .execute(params![
                            format!("folder-{index}"),
                            format!("Folder {index}")
                        ])
                        .unwrap();
                }
            }
            tx.commit().unwrap();
        }

        let mut inserted = 0usize;
        for target in [10_000usize, 30_000, 100_000, 250_000] {
            let insert_started = Instant::now();
            {
                let mut connection = repo.connection.lock().unwrap();
                let tx = connection.transaction().unwrap();
                {
                    let mut file = tx
                        .prepare(
                            "INSERT INTO files(
                               id,name,extension,kind,size_bytes,updated_at,
                               favorite,trashed,folder,tags_json,provider,telegram_message_id
                             ) VALUES(?1,?2,'txt','document',?3,?4,0,0,'Mi unidad','[]','telegram',?5)",
                        )
                        .unwrap();
                    let mut location = tx
                        .prepare("INSERT INTO file_locations(file_id,folder_id) VALUES(?1,?2)")
                        .unwrap();
                    for index in inserted..target {
                        let id = format!("perf-{index}");
                        file.execute(params![
                            id,
                            format!("Benchfile {index}"),
                            1024_i64 + (index % 8192) as i64,
                            format!("2026-09-18T12:{:02}:{:02}Z", index % 60, index % 60),
                            (index + 1).to_string(),
                        ])
                        .unwrap();
                        location
                            .execute(params![
                                format!("perf-{index}"),
                                format!("folder-{}", index % 1000)
                            ])
                            .unwrap();
                    }
                }
                tx.commit().unwrap();
            }
            inserted = target;
            repo.optimize().unwrap();
            let insert_ms = elapsed_ms(insert_started);

            let page_started = Instant::now();
            let page = repo
                .list_files_page(CatalogPageQuery {
                    section: "recent".into(),
                    folder_id: None,
                    kind: None,
                    search: String::new(),
                    tag: None,
                    sort: "newest".into(),
                    offset: 0,
                    limit: 80,
                })
                .unwrap();
            let page_ms = elapsed_ms(page_started);
            assert_eq!(page.files.len(), 80);
            assert_eq!(page.total, target);

            let search_started = Instant::now();
            let search = repo
                .list_files_page(CatalogPageQuery {
                    section: "files".into(),
                    folder_id: None,
                    kind: None,
                    search: format!("benchfile {}", target - 1),
                    tag: None,
                    sort: "newest".into(),
                    offset: 0,
                    limit: 80,
                })
                .unwrap();
            let search_ms = elapsed_ms(search_started);
            assert!(!search.files.is_empty());

            let stats_started = Instant::now();
            let (_, file_count, _, _) = repo.catalog_stats().unwrap();
            let stats_ms = elapsed_ms(stats_started);
            assert_eq!(file_count, target);

            let folders_started = Instant::now();
            let folders = repo.list_folders().unwrap();
            let folders_ms = elapsed_ms(folders_started);
            assert_eq!(folders.len(), 1000);

            let cursor = repo.catalog_change_cursor().unwrap();
            {
                let connection = repo.connection.lock().unwrap();
                connection
                    .execute(
                        "UPDATE files SET favorite=1 WHERE id=?1",
                        [format!("perf-{}", target - 1)],
                    )
                    .unwrap();
            }
            let delta_started = Instant::now();
            let delta = repo.sync_file_delta(cursor).unwrap();
            let delta_ms = elapsed_ms(delta_started);
            assert_eq!(delta.files.len(), 1);

            eprintln!(
                "catalog_perf files={target} insert_ms={insert_ms} page_ms={page_ms} search_ms={search_ms} stats_ms={stats_ms} folders_ms={folders_ms} delta_ms={delta_ms}"
            );

            assert!(page_ms <= 1_500, "{target}: first page took {page_ms} ms");
            assert!(
                search_ms <= 1_500,
                "{target}: FTS search took {search_ms} ms"
            );
            assert!(
                stats_ms <= 1_000,
                "{target}: catalog stats took {stats_ms} ms"
            );
            assert!(
                folders_ms <= 1_500,
                "{target}: folder aggregates took {folders_ms} ms"
            );
            assert!(
                delta_ms <= 500,
                "{target}: one-file delta took {delta_ms} ms"
            );
        }
    }

    #[test]
    fn catalog_name_sort_is_natural_for_numeric_runs() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        {
            let connection = repo.connection.lock().unwrap();
            connection
                .execute_batch(
                    r#"
                    INSERT INTO files(id,name,extension,kind,size_bytes,updated_at)
                    VALUES
                      ('ten','Archivo 10','wav','audio',10,'2026-01-02T00:00:00Z'),
                      ('two','Archivo 2','wav','audio',20,'2026-01-01T00:00:00Z');
                    "#,
                )
                .unwrap();
        }
        let page = repo
            .list_files_page(CatalogPageQuery {
                section: "recent".into(),
                folder_id: None,
                kind: Some("audio".into()),
                search: String::new(),
                tag: None,
                sort: "name".into(),
                offset: 0,
                limit: 80,
            })
            .unwrap();
        assert_eq!(
            page.files
                .iter()
                .map(|file| file.id.as_str())
                .collect::<Vec<_>>(),
            vec!["two", "ten"]
        );
    }

    #[test]
    fn catalog_pages_preserve_filters_search_sort_and_counts() {
        let root = tempfile::tempdir().unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        repo.create_folder_local("folder-a", "Fotos", None).unwrap();
        {
            let connection = repo.connection.lock().unwrap();
            connection.execute_batch(
                r#"
                INSERT INTO files(id,name,extension,kind,size_bytes,updated_at,favorite,trashed,tags_json)
                VALUES
                  ('a','needle-photo','jpg','image',50,'2026-01-05T00:00:00Z',1,0,'["Fotos"]'),
                  ('b','report','pdf','pdf',40,'2026-01-04T00:00:00Z',0,0,'["Trabajo"]'),
                  ('c','root-note','txt','text',30,'2026-01-03T00:00:00Z',0,0,'["Personal"]'),
                  ('d','old-trash','zip','archive',20,'2026-01-02T00:00:00Z',1,1,'[]'),
                  ('e','small','jpg','image',10,'2026-01-01T00:00:00Z',0,0,'[]');
                INSERT INTO file_locations(file_id,folder_id) VALUES ('a','folder-a');
                INSERT INTO file_locations(file_id,folder_id) VALUES ('b','folder-a');
                INSERT INTO file_locations(file_id,folder_id) VALUES ('e','folder-a');
                "#,
            ).unwrap();
        }

        let root_page = repo
            .list_files_page(CatalogPageQuery {
                section: "files".into(),
                folder_id: None,
                kind: None,
                search: String::new(),
                tag: None,
                sort: "recent".into(),
                offset: 0,
                limit: 80,
            })
            .unwrap();
        assert_eq!(root_page.total, 1);
        assert_eq!(root_page.files[0].id, "c");

        let global_search = repo
            .list_files_page(CatalogPageQuery {
                section: "files".into(),
                folder_id: None,
                kind: None,
                search: "eed".into(),
                tag: None,
                sort: "recent".into(),
                offset: 0,
                limit: 80,
            })
            .unwrap();
        assert_eq!(global_search.total, 1);
        assert_eq!(global_search.files[0].id, "a");

        let favorites = repo
            .list_files_page(CatalogPageQuery {
                section: "favorites".into(),
                folder_id: None,
                kind: None,
                search: String::new(),
                tag: None,
                sort: "recent".into(),
                offset: 0,
                limit: 80,
            })
            .unwrap();
        assert_eq!(favorites.total, 1);
        assert_eq!(favorites.files[0].id, "a");

        let images = repo
            .list_files_page(CatalogPageQuery {
                section: "recent".into(),
                folder_id: None,
                kind: Some("image".into()),
                search: String::new(),
                tag: None,
                sort: "size".into(),
                offset: 0,
                limit: 1,
            })
            .unwrap();
        assert_eq!(images.total, 2);
        assert!(images.has_more);
        assert_eq!(images.files[0].id, "a");

        let tagged = repo
            .list_files_page(CatalogPageQuery {
                section: "recent".into(),
                folder_id: None,
                kind: None,
                search: String::new(),
                tag: Some("Trabajo".into()),
                sort: "recent".into(),
                offset: 0,
                limit: 80,
            })
            .unwrap();
        assert_eq!(tagged.total, 1);
        assert_eq!(tagged.files[0].id, "b");

        let (_, active, favorite_count, trash_count) = repo.catalog_stats().unwrap();
        assert_eq!(active, 4);
        assert_eq!(favorite_count, 1);
        assert_eq!(trash_count, 1);
    }

    #[test]
    fn upload_concurrency_migrates_once_and_preserves_user_choices() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        let repo = CatalogRepository::open(&path).unwrap();
        assert_eq!(repo.settings().unwrap().upload_concurrency, 4);

        // Simulate an installation that already received the old v2 default of 8 but
        // has not run the v3 migration yet.
        repo.set_setting("upload_concurrency", "8").unwrap();
        repo.connection
            .lock()
            .unwrap()
            .execute("DELETE FROM app_meta WHERE key='upload_concurrency_v3'", [])
            .unwrap();
        drop(repo);
        let repo = CatalogRepository::open(&path).unwrap();
        assert_eq!(repo.settings().unwrap().upload_concurrency, 4);

        for value in [1, 4, 8, 16] {
            repo.set_setting("upload_concurrency", &value.to_string())
                .unwrap();
            assert_eq!(repo.settings().unwrap().upload_concurrency, value);
        }
        // Once v3 has run, a deliberate user choice of 8 must stay at 8.
        repo.set_setting("upload_concurrency", "8").unwrap();
        drop(repo);
        assert_eq!(
            CatalogRepository::open(&path)
                .unwrap()
                .settings()
                .unwrap()
                .upload_concurrency,
            8
        );
    }

    #[test]
    fn resource_profile_defaults_to_balanced_and_persists() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        let repo = CatalogRepository::open(&path).unwrap();
        assert_eq!(repo.settings().unwrap().resource_profile, "balanced");
        repo.set_setting("resource_profile", "low").unwrap();
        assert_eq!(repo.settings().unwrap().resource_profile, "low");
        drop(repo);
        assert_eq!(
            CatalogRepository::open(&path)
                .unwrap()
                .settings()
                .unwrap()
                .resource_profile,
            "low"
        );
    }
}
