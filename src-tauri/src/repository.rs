use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

use crate::domain::{AppSettings, CloudFile, CloudFolder, QueueSummary, TransferJob};

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub struct CatalogRepository {
    pub(crate) connection: Mutex<Connection>,
}

#[derive(Debug, Clone, Copy)]
pub struct TransferControl {
    pub pause_requested: bool,
    pub cancel_requested: bool,
}

impl CatalogRepository {
    pub fn open(path: &Path) -> Result<Self, RepositoryError> {
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;

        let repository = Self {
            connection: Mutex::new(connection),
        };
        repository.migrate()?;
        repository.remove_demo()?;
        Ok(repository)
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

            CREATE TABLE IF NOT EXISTS transfer_metadata (
                transfer_id TEXT PRIMARY KEY NOT NULL,
                local_path TEXT NOT NULL,
                size_bytes INTEGER NOT NULL DEFAULT 0,
                sha256 TEXT NOT NULL,
                encrypted INTEGER NOT NULL DEFAULT 0,
                remote_message_id TEXT NULL,
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

            CREATE TABLE IF NOT EXISTS app_meta (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS app_settings (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            INSERT OR IGNORE INTO app_settings VALUES ('preparation_concurrency','4');
            INSERT OR IGNORE INTO app_settings VALUES ('upload_concurrency','8');
            UPDATE app_settings SET value='8' WHERE key='upload_concurrency' AND value='4'
            AND NOT EXISTS (SELECT 1 FROM app_meta WHERE key='upload_concurrency_v2');
            INSERT OR IGNORE INTO app_meta VALUES ('upload_concurrency_v2','1');
            INSERT OR IGNORE INTO app_settings VALUES ('download_concurrency','2');
            INSERT OR IGNORE INTO app_settings VALUES ('cache_limit_bytes','2147483648');
            INSERT OR IGNORE INTO app_settings VALUES ('remember_session','0');
            INSERT OR IGNORE INTO app_settings VALUES ('conflict_policy','skip');
            INSERT OR IGNORE INTO app_settings VALUES ('speed_limit_bps','');

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
        let connection = self.connection.lock().expect("catalog mutex poisoned");
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
            let tags = serde_json::from_str::<Vec<String>>(&tags_json).unwrap_or_default();
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

    pub fn list_folders(&self) -> Result<Vec<CloudFolder>, RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let mut statement = connection.prepare(
            r#"SELECT f.id, f.name, f.parent_id, f.trashed, f.created_at, f.updated_at,
                      (SELECT COUNT(*) FROM file_locations fl JOIN files fi ON fi.id=fl.file_id WHERE fl.folder_id=f.id AND fi.trashed=0),
                      (SELECT COUNT(*) FROM folders child WHERE child.parent_id=f.id AND child.trashed=0),
                      COALESCE((SELECT SUM(fi.size_bytes) FROM file_locations fl JOIN files fi ON fi.id=fl.file_id WHERE fl.folder_id=f.id AND fi.trashed=0),0)
               FROM folders f
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
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let mut statement = connection.prepare(
            r#"SELECT t.id,t.file_name,t.direction,t.progress,t.status,t.speed_label,
                      COALESCE(r.phase,t.status),COALESCE(r.processed_bytes,0),
                      COALESCE(r.total_bytes,m.size_bytes,0),COALESCE(r.speed_bps,0),r.eta_seconds,
                      COALESCE(r.attempts,0),COALESCE(r.max_attempts,5),m.error,r.started_at,
                      COALESCE(r.updated_at,m.created_at,0)
               FROM transfers t
               LEFT JOIN transfer_metadata m ON m.transfer_id=t.id
               LEFT JOIN transfer_runtime r ON r.transfer_id=t.id
               ORDER BY COALESCE(r.updated_at,m.created_at,0) DESC,t.rowid DESC"#,
        )?;
        let rows = statement.query_map([], |row| {
            let raw_progress: i64 = row.get(3)?;
            let status: String = row.get(4)?;
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
            Ok(TransferJob {
                id: row.get(0)?,
                file_name: row.get(1)?,
                direction: row.get(2)?,
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
                started_at: row.get(14)?,
                updated_at: row.get(15)?,
            })
        })?;

        let mut transfers = Vec::new();
        for row in rows {
            transfers.push(row?);
        }
        Ok(transfers)
    }

    pub fn queue_summary(&self, cache_bytes: i64) -> Result<QueueSummary, RepositoryError> {
        let all_jobs = self.list_transfers()?;
        let completed = all_jobs
            .iter()
            .filter(|job| matches!(job.status.as_str(), "completed" | "duplicate"))
            .count();
        let jobs: Vec<_> = all_jobs
            .into_iter()
            .filter(|job| !matches!(job.status.as_str(), "completed" | "duplicate" | "cancelled"))
            .collect();
        let settings = self.settings()?;
        let total = jobs.len();
        let failed = jobs.iter().filter(|j| j.status == "failed").count();
        let active = jobs
            .iter()
            .filter(|j| {
                matches!(
                    j.status.as_str(),
                    "analyzing"
                        | "copying"
                        | "uploading"
                        | "downloading"
                        | "confirming"
                        | "running"
                )
            })
            .count();
        let pending = jobs
            .iter()
            .filter(|j| {
                !matches!(
                    j.status.as_str(),
                    "completed" | "duplicate" | "failed" | "cancelled"
                )
            })
            .count();
        let total_bytes: i64 = jobs
            .iter()
            .filter(|j| j.status != "cancelled")
            .map(|j| j.total_bytes.max(0))
            .sum();
        let processed_bytes: i64 = jobs
            .iter()
            .filter(|j| j.status != "cancelled")
            .map(|j| j.processed_bytes.clamp(0, j.total_bytes.max(0)))
            .sum();
        let speed_bps: i64 = jobs
            .iter()
            .filter(|j| {
                matches!(
                    j.status.as_str(),
                    "uploading" | "downloading" | "copying" | "analyzing" | "running"
                )
            })
            .map(|j| j.speed_bps.max(0))
            .sum();
        let remaining = (total_bytes - processed_bytes).max(0);
        let eta_seconds = if speed_bps > 0 {
            Some((remaining + speed_bps - 1) / speed_bps)
        } else {
            None
        };
        Ok(QueueSummary {
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
            cache_limit_bytes: settings.cache_limit_bytes,
        })
    }

    pub fn settings(&self) -> Result<AppSettings, RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let get = |key: &str| -> Result<Option<String>, rusqlite::Error> {
            connection
                .query_row("SELECT value FROM app_settings WHERE key=?1", [key], |r| {
                    r.get(0)
                })
                .optional()
        };
        let defaults = AppSettings::default();
        let prep = get("preparation_concurrency")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(defaults.preparation_concurrency)
            .clamp(1, 8);
        let upload = get("upload_concurrency")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(defaults.upload_concurrency)
            .clamp(1, 16);
        let download = get("download_concurrency")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(defaults.download_concurrency)
            .clamp(1, 8);
        let cache = get("cache_limit_bytes")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(defaults.cache_limit_bytes)
            .clamp(256 * 1024 * 1024, 20 * 1024 * 1024 * 1024_i64);
        let remember = get("remember_session")?.map(|v| v == "1").unwrap_or(false);
        let conflict = get("conflict_policy")?
            .filter(|v| v == "skip" || v == "rename")
            .unwrap_or(defaults.conflict_policy);
        let speed_limit_bps = get("speed_limit_bps")?
            .and_then(|v| {
                if v.is_empty() {
                    None
                } else {
                    v.parse::<i64>().ok()
                }
            })
            .filter(|v| *v > 0);
        Ok(AppSettings {
            preparation_concurrency: prep,
            upload_concurrency: upload,
            download_concurrency: download,
            cache_limit_bytes: cache,
            remember_session: remember,
            conflict_policy: conflict,
            speed_limit_bps,
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
            "speed_limit_bps",
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
            id, file_name, local_path, size_bytes, sha256, encrypted, status, None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn enqueue_transfer_with_folder(
        &self,
        id: &str,
        file_name: &str,
        local_path: &str,
        size_bytes: i64,
        sha256: &str,
        encrypted: bool,
        status: &str,
        folder_id: Option<&str>,
    ) -> Result<(), RepositoryError> {
        let mut connection = self.connection.lock().expect("catalog mutex poisoned");
        let transaction = connection.transaction()?;
        ensure_folder_parent(&transaction, folder_id, None)?;
        transaction.execute(
            "INSERT INTO transfers (id,file_name,direction,progress,status,speed_label) VALUES (?1,?2,'upload',0,?3,'Esperando')",
            params![id, file_name, status],
        )?;
        transaction.execute(
            "INSERT INTO transfer_metadata (transfer_id,local_path,size_bytes,sha256,encrypted,created_at) VALUES (?1,?2,?3,?4,?5,unixepoch())",
            params![id, local_path, size_bytes, sha256, if encrypted { 1 } else { 0 }],
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

    pub fn create_upload_placeholder_in_folder(
        &self,
        id: &str,
        file_name: &str,
        source_path: &str,
        size_bytes: i64,
        folder_id: Option<&str>,
    ) -> Result<(), RepositoryError> {
        self.enqueue_transfer_with_folder(
            id,
            file_name,
            source_path,
            size_bytes,
            "",
            false,
            "waiting",
            folder_id,
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
        let progress = if total_bytes > 0 {
            (processed_bytes.saturating_mul(100) / total_bytes).clamp(0, 100)
        } else {
            0
        };
        let mut connection = self.connection.lock().expect("catalog mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE transfers SET status=?1,progress=?2,speed_label=?3 WHERE id=?4",
            params![status, progress, speed_label, id],
        )?;
        transaction.execute(
            "UPDATE transfer_metadata SET error=?1 WHERE transfer_id=?2",
            params![error, id],
        )?;
        transaction.execute(
            "UPDATE transfer_runtime SET phase=?1,processed_bytes=?2,total_bytes=?3,speed_bps=?4,eta_seconds=?5,started_at=COALESCE(started_at,unixepoch()),updated_at=unixepoch(),completed_at=CASE WHEN ?6='completed' THEN unixepoch() ELSE completed_at END WHERE transfer_id=?7",
            params![phase,processed_bytes.max(0),total_bytes.max(0),speed_bps.max(0),eta_seconds,status,id],
        )?;
        transaction.commit()?;
        Ok(())
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
        transaction.commit()?;
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

    pub fn active_count(&self) -> Result<usize, RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let count:i64=connection.query_row("SELECT COUNT(*) FROM transfers WHERE status IN ('analyzing','copying','uploading','downloading','confirming','running')",[],|r|r.get(0))?;
        Ok(count.max(0) as usize)
    }

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
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        let changed = connection.execute(
            "DELETE FROM transfers WHERE status IN ('completed','duplicate','cancelled')",
            [],
        )?;
        Ok(changed)
    }

    pub fn pause_all_active(&self) -> Result<(), RepositoryError> {
        let connection = self.connection.lock().expect("catalog mutex poisoned");
        connection.execute("UPDATE transfer_runtime SET pause_requested=1,updated_at=unixepoch() WHERE transfer_id IN (SELECT id FROM transfers WHERE status IN ('analyzing','copying','uploading','downloading','confirming','running'))",[])?;
        Ok(())
    }
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
        assert_eq!(repo.list_transfers().unwrap().len(), 301);
        let summary = repo.queue_summary(0).unwrap();
        assert_eq!(summary.pending, 301);
        assert_eq!(summary.total_bytes, 3010);
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
            3,
            Some("folder-a"),
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
                3,
                Some("missing")
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
    fn upload_concurrency_migrates_once_and_preserves_user_choices() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        let repo = CatalogRepository::open(&path).unwrap();
        assert_eq!(repo.settings().unwrap().upload_concurrency, 8);
        repo.set_setting("upload_concurrency", "4").unwrap();
        repo.connection
            .lock()
            .unwrap()
            .execute("DELETE FROM app_meta WHERE key='upload_concurrency_v2'", [])
            .unwrap();
        drop(repo);
        let repo = CatalogRepository::open(&path).unwrap();
        assert_eq!(repo.settings().unwrap().upload_concurrency, 8);
        for value in [1, 4, 8, 16] {
            repo.set_setting("upload_concurrency", &value.to_string())
                .unwrap();
            assert_eq!(repo.settings().unwrap().upload_concurrency, value);
        }
        repo.set_setting("upload_concurrency", "4").unwrap();
        drop(repo);
        assert_eq!(
            CatalogRepository::open(&path)
                .unwrap()
                .settings()
                .unwrap()
                .upload_concurrency,
            4
        );
    }
}
