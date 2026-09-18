use std::fs;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::crypto::encrypt_file;
use crate::progress::SpeedEstimator;
use crate::repository::CatalogRepository;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedUpload {
    pub transfer_id: String,
    pub file_name: String,
    pub local_path: String,
    pub size_bytes: i64,
    pub sha256: String,
    pub duplicate: bool,
    pub encrypted: bool,
    pub status: String,
}

pub struct TransferService;

impl TransferService {
    #[cfg(test)]
    pub fn prepare_upload(
        repository: &CatalogRepository,
        path: &str,
        encrypt: bool,
        passphrase: Option<String>,
        staging_dir: &Path,
    ) -> Result<PreparedUpload, String> {
        Self::prepare_upload_in_folder(
            repository,
            path,
            Some(path),
            encrypt,
            passphrase,
            staging_dir,
            None,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_upload_in_folder(
        repository: &CatalogRepository,
        path: &str,
        original_source_path: Option<&str>,
        encrypt: bool,
        passphrase: Option<String>,
        staging_dir: &Path,
        folder_id: Option<&str>,
        delete_source_after_upload: bool,
    ) -> Result<PreparedUpload, String> {
        let source_path = normalize_path(path)?;
        let metadata = fs::metadata(&source_path).map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("La selección no es un archivo".to_string());
        }
        let size_bytes = i64::try_from(metadata.len())
            .map_err(|_| "El archivo es demasiado grande para indexarlo".to_string())?;
        if size_bytes <= 0 {
            return Err("Telegram no permite subir archivos vacíos".to_string());
        }
        let file_name = source_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "No se pudo determinar el nombre del archivo".to_string())?
            .to_string();
        let transfer_id = new_transfer_id();

        repository
            .create_upload_placeholder_in_folder(
                &transfer_id,
                &file_name,
                &source_path.to_string_lossy(),
                original_source_path,
                size_bytes,
                folder_id,
                delete_source_after_upload,
            )
            .map_err(|error| error.to_string())?;

        Self::prepare_existing(
            repository,
            transfer_id,
            file_name,
            source_path,
            size_bytes,
            encrypt,
            passphrase,
            staging_dir,
        )
    }

    pub fn adopt_generated_upload_in_folder(
        repository: &CatalogRepository,
        path: &str,
        staging_dir: &Path,
        folder_id: Option<&str>,
    ) -> Result<PreparedUpload, String> {
        let source_path = normalize_path(path)?;
        let metadata = fs::metadata(&source_path).map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("La selección generada no es un archivo".to_string());
        }
        let size_bytes = i64::try_from(metadata.len())
            .map_err(|_| "El archivo es demasiado grande para indexarlo".to_string())?;
        if size_bytes <= 0 {
            return Err("Telegram no permite subir archivos vacíos".to_string());
        }

        let file_name = source_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "No se pudo determinar el nombre del archivo".to_string())?
            .to_string();
        let transfer_id = new_transfer_id();
        let directory = staging_dir.join(&transfer_id);
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let snapshot = directory.join(&file_name);

        fs::rename(&source_path, &snapshot).map_err(|error| {
            let _ = fs::remove_dir(&directory);
            format!("No se pudo adoptar el ZIP generado en la caché privada: {error}")
        })?;

        if let Err(error) = repository.create_upload_placeholder_in_folder(
            &transfer_id,
            &file_name,
            &snapshot.to_string_lossy(),
            None,
            size_bytes,
            folder_id,
            false,
        ) {
            let _ = fs::rename(&snapshot, &source_path);
            let _ = fs::remove_dir(&directory);
            return Err(error.to_string());
        }

        if let Err(error) = repository.update_runtime(
            &transfer_id,
            "analyzing",
            "analyzing",
            0,
            size_bytes,
            0,
            None,
            "Verificando ZIP generado",
            None,
        ) {
            let error = error.to_string();
            mark_preparation_failure(
                repository,
                &transfer_id,
                size_bytes,
                "Error al preparar ZIP generado",
                &error,
            );
            return Err(error);
        }

        let sha256 = match hash_with_progress(repository, &transfer_id, &snapshot, size_bytes) {
            Ok(hash) => hash,
            Err(error) => {
                mark_preparation_failure(
                    repository,
                    &transfer_id,
                    size_bytes,
                    "Error al verificar ZIP generado",
                    &error,
                );
                return Err(error);
            }
        };

        let duplicate = match repository.finish_preparation(
            &transfer_id,
            &snapshot.to_string_lossy(),
            &sha256,
            size_bytes,
            false,
        ) {
            Ok(value) => value,
            Err(error) => {
                let error = error.to_string();
                mark_preparation_failure(
                    repository,
                    &transfer_id,
                    size_bytes,
                    "Error al registrar ZIP generado",
                    &error,
                );
                return Err(error);
            }
        };

        if duplicate {
            let _ = fs::remove_file(&snapshot);
        }

        Ok(PreparedUpload {
            transfer_id,
            file_name,
            local_path: snapshot.to_string_lossy().into_owned(),
            size_bytes,
            sha256,
            duplicate,
            encrypted: false,
            status: if duplicate { "duplicate" } else { "ready" }.to_string(),
        })
    }

    #[cfg(any(target_os = "android", test))]
    #[allow(clippy::too_many_arguments)]
    pub fn adopt_preverified_upload_in_folder(
        repository: &CatalogRepository,
        path: &str,
        original_source_path: &str,
        size_bytes: i64,
        sha256: &str,
        staging_dir: &Path,
        folder_id: Option<&str>,
        delete_source_after_upload: bool,
    ) -> Result<PreparedUpload, String> {
        if size_bytes <= 0 {
            return Err("Telegram no permite subir archivos vacíos".to_string());
        }
        if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Android devolvió una huella SHA-256 inválida".to_string());
        }

        let source_path = normalize_path(path)?;
        let metadata = fs::metadata(&source_path).map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() != size_bytes as u64 {
            return Err(
                "El archivo preparado por Android no coincide con su tamaño verificado".into(),
            );
        }
        let file_name = source_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "No se pudo determinar el nombre del archivo".to_string())?
            .to_string();

        let transfer_id = new_transfer_id();
        let directory = staging_dir.join(&transfer_id);
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let snapshot = directory.join(&file_name);

        fs::rename(&source_path, &snapshot).map_err(|error| {
            let _ = fs::remove_dir(&directory);
            format!("No se pudo adoptar el archivo Android en la caché privada: {error}")
        })?;

        if let Err(error) = repository.create_upload_placeholder_in_folder(
            &transfer_id,
            &file_name,
            &snapshot.to_string_lossy(),
            Some(original_source_path),
            size_bytes,
            folder_id,
            delete_source_after_upload,
        ) {
            let _ = fs::rename(&snapshot, &source_path);
            let _ = fs::remove_dir(&directory);
            return Err(error.to_string());
        }

        let duplicate = match repository.finish_preparation(
            &transfer_id,
            &snapshot.to_string_lossy(),
            sha256,
            size_bytes,
            false,
        ) {
            Ok(value) => value,
            Err(error) => {
                let error = error.to_string();
                mark_preparation_failure(
                    repository,
                    &transfer_id,
                    size_bytes,
                    "Error al registrar archivo Android",
                    &error,
                );
                return Err(error);
            }
        };
        if duplicate {
            let _ = fs::remove_file(&snapshot);
        }

        Ok(PreparedUpload {
            transfer_id,
            file_name,
            local_path: snapshot.to_string_lossy().into_owned(),
            size_bytes,
            sha256: sha256.to_ascii_lowercase(),
            duplicate,
            encrypted: false,
            status: if duplicate { "duplicate" } else { "ready" }.to_string(),
        })
    }

    pub fn resume_preparation(
        repository: &CatalogRepository,
        transfer_id: &str,
        staging_dir: &Path,
    ) -> Result<PreparedUpload, String> {
        let Some((file_name, source, expected_size)) = repository
            .preparation_source(transfer_id)
            .map_err(|error| error.to_string())?
        else {
            return Err("La transferencia ya no necesita preparación".to_string());
        };

        let source_path = normalize_path(&source)?;
        let metadata = fs::metadata(&source_path).map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() != expected_size as u64 {
            return Err(
                "El archivo original cambió o ya no está disponible. Selecciónalo de nuevo."
                    .to_string(),
            );
        }

        repository
            .reset_preparation(transfer_id)
            .map_err(|error| error.to_string())?;

        Self::prepare_existing(
            repository,
            transfer_id.to_string(),
            file_name,
            source_path,
            expected_size,
            false,
            None,
            staging_dir,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_existing(
        repository: &CatalogRepository,
        transfer_id: String,
        file_name: String,
        source_path: PathBuf,
        size_bytes: i64,
        encrypt: bool,
        passphrase: Option<String>,
        staging_dir: &Path,
    ) -> Result<PreparedUpload, String> {
        let mut passphrase = passphrase.map(zeroize::Zeroizing::new);
        repository
            .update_runtime(
                &transfer_id,
                "analyzing",
                "analyzing",
                0,
                size_bytes,
                0,
                None,
                "Analizando archivo",
                None,
            )
            .map_err(|error| error.to_string())?;

        if !encrypt {
            let result: Result<PreparedUpload, String> = (|| {
                let directory = staging_dir.join(&transfer_id);
                fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
                let snapshot = directory.join(&file_name);
                let sha256 = if source_path == snapshot {
                    // Generated archives can already live in their final private
                    // staging location. Re-hash them in place instead of copying
                    // the file over itself (also makes paused adoption resumable).
                    hash_with_progress(repository, &transfer_id, &source_path, size_bytes)?
                } else {
                    copy_with_progress(
                        repository,
                        &transfer_id,
                        &source_path,
                        &snapshot,
                        size_bytes,
                        "",
                    )?
                };
                let duplicate = repository
                    .finish_preparation(
                        &transfer_id,
                        &snapshot.to_string_lossy(),
                        &sha256,
                        size_bytes,
                        false,
                    )
                    .map_err(|e| e.to_string())?;
                if duplicate {
                    let _ = fs::remove_file(&snapshot);
                }
                Ok(PreparedUpload {
                    transfer_id: transfer_id.clone(),
                    file_name: file_name.clone(),
                    local_path: if duplicate {
                        source_path.to_string_lossy().into_owned()
                    } else {
                        snapshot.to_string_lossy().into_owned()
                    },
                    size_bytes,
                    sha256,
                    duplicate,
                    encrypted: false,
                    status: if duplicate { "duplicate" } else { "ready" }.into(),
                })
            })();
            if let Err(error) = &result {
                mark_preparation_failure(
                    repository,
                    &transfer_id,
                    size_bytes,
                    "Error al preparar",
                    error,
                );
            }
            return result;
        }
        let sha256 = match hash_with_progress(repository, &transfer_id, &source_path, size_bytes) {
            Ok(hash) => hash,
            Err(error) => {
                mark_preparation_failure(
                    repository,
                    &transfer_id,
                    size_bytes,
                    "Error al analizar",
                    &error,
                );
                return Err(error);
            }
        };
        let duplicate = repository
            .has_hash(&sha256)
            .map_err(|error| error.to_string())?;
        if duplicate {
            repository
                .finish_preparation(
                    &transfer_id,
                    &source_path.to_string_lossy(),
                    &sha256,
                    size_bytes,
                    true,
                )
                .map_err(|error| error.to_string())?;
            return Ok(PreparedUpload {
                transfer_id,
                file_name,
                local_path: source_path.to_string_lossy().into_owned(),
                size_bytes,
                sha256,
                duplicate: true,
                encrypted: false,
                status: "duplicate".to_string(),
            });
        }

        repository
            .update_runtime(
                &transfer_id,
                "copying",
                "copying",
                0,
                size_bytes,
                0,
                None,
                "Copiando a caché privada",
                None,
            )
            .map_err(|error| error.to_string())?;

        let directory = staging_dir.join(&transfer_id);
        if let Err(error) = fs::create_dir_all(&directory) {
            let error = error.to_string();
            mark_preparation_failure(
                repository,
                &transfer_id,
                size_bytes,
                "No se pudo crear la caché privada",
                &error,
            );
            return Err(error);
        }

        let prepared_path = if encrypt {
            let password = passphrase
                .as_deref()
                .map(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "Se necesita una contraseña para cifrar el archivo".to_string())?;
            let encrypted_path = directory.join(format!("{file_name}.nuv"));
            if let Err(error) = encrypt_file(&source_path, &encrypted_path, password) {
                let error = error.to_string();
                mark_preparation_failure(
                    repository,
                    &transfer_id,
                    size_bytes,
                    "Error al cifrar",
                    &error,
                );
                return Err(error);
            }
            repository
                .update_runtime(
                    &transfer_id,
                    "copying",
                    "copying",
                    size_bytes,
                    size_bytes,
                    0,
                    Some(0),
                    "Cifrado preparado",
                    None,
                )
                .map_err(|error| error.to_string())?;
            encrypted_path
        } else {
            let snapshot = directory.join(&file_name);
            if let Err(error) = copy_with_progress(
                repository,
                &transfer_id,
                &source_path,
                &snapshot,
                size_bytes,
                &sha256,
            ) {
                mark_preparation_failure(
                    repository,
                    &transfer_id,
                    size_bytes,
                    "Error al copiar",
                    &error,
                );
                return Err(error);
            }
            snapshot
        };

        passphrase.take();
        let duplicate = repository
            .finish_preparation(
                &transfer_id,
                &prepared_path.to_string_lossy(),
                &sha256,
                size_bytes,
                false,
            )
            .map_err(|error| error.to_string())?;

        Ok(PreparedUpload {
            transfer_id,
            file_name,
            local_path: prepared_path.to_string_lossy().into_owned(),
            size_bytes,
            sha256,
            duplicate,
            encrypted: encrypt,
            status: if duplicate { "duplicate" } else { "ready" }.to_string(),
        })
    }
}

fn mark_preparation_failure(
    repository: &CatalogRepository,
    transfer_id: &str,
    size_bytes: i64,
    label: &str,
    error: &str,
) {
    if matches!(error, "Transferencia pausada" | "Transferencia cancelada") {
        return;
    }
    let _ = repository.update_runtime(
        transfer_id,
        "failed",
        "error",
        0,
        size_bytes,
        0,
        None,
        label,
        Some(error),
    );
}

fn hash_with_progress(
    repository: &CatalogRepository,
    transfer_id: &str,
    path: &Path,
    total: i64,
) -> Result<String, String> {
    let mut reader = BufReader::with_capacity(
        2 * 1024 * 1024,
        fs::File::open(path).map_err(|e| e.to_string())?,
    );
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut processed = 0_i64;
    let mut estimator = SpeedEstimator::new(0);
    let mut last_report = Instant::now() - Duration::from_secs(1);

    loop {
        let read = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        processed += read as i64;
        if last_report.elapsed() >= Duration::from_millis(250) || processed >= total {
            check_control(repository, transfer_id)?;
            let speed = estimator.update(processed);
            let eta = SpeedEstimator::eta(total, processed, speed);
            repository
                .update_runtime(
                    transfer_id,
                    "analyzing",
                    "analyzing",
                    processed,
                    total,
                    speed,
                    eta,
                    "Analizando y calculando SHA-256",
                    None,
                )
                .map_err(|e| e.to_string())?;
            last_report = Instant::now();
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

fn copy_with_progress(
    repository: &CatalogRepository,
    transfer_id: &str,
    source: &Path,
    destination: &Path,
    total: i64,
    expected_hash: &str,
) -> Result<String, String> {
    let before = fs::metadata(source).map_err(|e| e.to_string())?;
    let mut reader = BufReader::with_capacity(
        2 * 1024 * 1024,
        fs::File::open(source).map_err(|e| e.to_string())?,
    );
    let mut writer = BufWriter::with_capacity(
        2 * 1024 * 1024,
        fs::File::create(destination).map_err(|e| e.to_string())?,
    );
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut processed = 0_i64;
    let mut estimator = SpeedEstimator::new(0);
    let mut last_report = Instant::now() - Duration::from_secs(1);

    loop {
        let read = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        writer
            .write_all(&buffer[..read])
            .map_err(|e| e.to_string())?;
        hasher.update(&buffer[..read]);
        processed += read as i64;
        if last_report.elapsed() >= Duration::from_millis(250) || processed >= total {
            check_control(repository, transfer_id)?;
            let speed = estimator.update(processed);
            repository
                .update_runtime(
                    transfer_id,
                    "copying",
                    "copying",
                    processed,
                    total,
                    speed,
                    SpeedEstimator::eta(total, processed, speed),
                    "Copiando a caché privada",
                    None,
                )
                .map_err(|e| e.to_string())?;
            last_report = Instant::now();
        }
    }
    writer.flush().map_err(|e| e.to_string())?;
    let hash = hex::encode(hasher.finalize());
    let after = fs::metadata(source).map_err(|e| e.to_string())?;
    if processed != total
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || (!expected_hash.is_empty() && hash != expected_hash)
    {
        let _ = fs::remove_file(destination);
        return Err("El archivo cambió mientras se preparaba. Inténtalo de nuevo.".to_string());
    }
    Ok(hash)
}

fn check_control(repository: &CatalogRepository, transfer_id: &str) -> Result<(), String> {
    let control = repository
        .transfer_control(transfer_id)
        .map_err(|e| e.to_string())?;
    if control.cancel_requested {
        repository
            .mark_cancelled(transfer_id)
            .map_err(|e| e.to_string())?;
        return Err("Transferencia cancelada".to_string());
    }
    if control.pause_requested {
        repository
            .mark_paused(transfer_id)
            .map_err(|e| e.to_string())?;
        return Err("Transferencia pausada".to_string());
    }
    Ok(())
}

pub(crate) fn normalize_path(input: &str) -> Result<PathBuf, String> {
    if input.trim().is_empty() {
        return Err("Ruta de archivo vacía".to_string());
    }
    if input.starts_with("content://") {
        return Err(
            "Android devolvió un URI no copiado; usa fileAccessMode='copy' en el selector"
                .to_string(),
        );
    }

    if input.starts_with("file://") {
        return tauri::Url::parse(input)
            .map_err(|_| "URL de archivo inválida".to_string())?
            .to_file_path()
            .map_err(|_| "La URL no corresponde a un archivo local".to_string());
    }

    Ok(Path::new(input).to_path_buf())
}

fn new_transfer_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let random: u64 = rand::random();
    format!("transfer-{now:x}-{random:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_url_decodes_spaces_and_unicode() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mi canción.txt");
        let url = tauri::Url::from_file_path(&path).unwrap();
        assert_eq!(normalize_path(url.as_str()).unwrap(), path);
    }

    #[test]
    fn staging_failure_is_reported_as_failed_transfer() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.txt");
        fs::write(&source, b"content").unwrap();
        let staging = root.path().join("not-a-directory");
        fs::write(&staging, b"occupied").unwrap();
        let repo = CatalogRepository::open(&root.path().join("db")).unwrap();
        assert!(TransferService::prepare_upload(
            &repo,
            source.to_str().unwrap(),
            false,
            None,
            &staging
        )
        .is_err());
        let job = repo.list_transfers().unwrap().remove(0);
        assert_eq!(job.status, "failed");
        assert!(job.error.is_some());
    }

    #[test]
    fn file_url_is_normalized() {
        #[cfg(target_os = "windows")]
        assert_eq!(
            normalize_path("file:///C:/tmp/a.txt").unwrap(),
            PathBuf::from("C:/tmp/a.txt")
        );
        #[cfg(not(target_os = "windows"))]
        assert_eq!(
            normalize_path("file:///tmp/a.txt").unwrap(),
            PathBuf::from("/tmp/a.txt")
        );
    }

    #[test]
    fn content_uri_is_rejected_without_copy_mode() {
        assert!(normalize_path("content://example/file").is_err());
    }

    #[test]
    fn prepare_upload_tracks_and_deduplicates() {
        let root = std::env::temp_dir().join(format!("nuvio-transfer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.txt");
        fs::write(&source, b"qa transfer").unwrap();
        let repository = CatalogRepository::open(&root.join("catalog.db")).unwrap();
        let staging = root.join("staging");

        let prepared = TransferService::prepare_upload(
            &repository,
            source.to_str().unwrap(),
            false,
            None,
            &staging,
        )
        .unwrap();
        assert_eq!(prepared.status, "ready");
        assert!(Path::new(&prepared.local_path).exists());
        let job = repository
            .list_transfers()
            .unwrap()
            .into_iter()
            .find(|t| t.id == prepared.transfer_id)
            .unwrap();
        assert_eq!(job.phase, "ready");
        assert_eq!(job.total_bytes, b"qa transfer".len() as i64);
        assert_eq!(job.processed_bytes, 0);

        let duplicate = TransferService::prepare_upload(
            &repository,
            source.to_str().unwrap(),
            false,
            None,
            &staging,
        )
        .unwrap();
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.status, "duplicate");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn freshly_prepared_staging_gets_one_shot_verification_token() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.bin");
        fs::write(&source, vec![7_u8; 1024 * 1024]).unwrap();
        let repository = CatalogRepository::open(&root.path().join("catalog.db")).unwrap();
        let staging = root.path().join("staging");

        let prepared = TransferService::prepare_upload(
            &repository,
            source.to_str().unwrap(),
            false,
            None,
            &staging,
        )
        .unwrap();

        assert!(repository.take_verified_staging(
            &prepared.transfer_id,
            &prepared.local_path,
            prepared.size_bytes,
        ));
        assert!(!repository.take_verified_staging(
            &prepared.transfer_id,
            &prepared.local_path,
            prepared.size_bytes,
        ));
    }

    #[test]
    fn preverified_android_staging_is_adopted_without_second_copy() {
        let root = tempfile::tempdir().unwrap();
        let incoming_dir = root.path().join("android-stage");
        fs::create_dir_all(&incoming_dir).unwrap();
        let incoming = incoming_dir.join("foto.jpg");
        let content = vec![3_u8; 512 * 1024];
        fs::write(&incoming, &content).unwrap();
        let sha256 = crate::crypto::sha256_file(&incoming).unwrap();

        let repository = CatalogRepository::open(&root.path().join("catalog.db")).unwrap();
        let staging = root.path().join("staging");
        let prepared = TransferService::adopt_preverified_upload_in_folder(
            &repository,
            incoming.to_str().unwrap(),
            "content://com.example/foto.jpg",
            content.len() as i64,
            &sha256,
            &staging,
            None,
            true,
        )
        .unwrap();

        assert!(!incoming.exists());
        let adopted = PathBuf::from(&prepared.local_path);
        assert!(adopted.exists());
        assert!(adopted.starts_with(&staging));
        assert_eq!(fs::read(&adopted).unwrap(), content);
        assert_eq!(prepared.sha256, sha256);
        assert!(repository.take_verified_staging(
            &prepared.transfer_id,
            &prepared.local_path,
            prepared.size_bytes,
        ));

        let source_path: String = repository
            .connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT source_path FROM transfer_metadata WHERE transfer_id=?1",
                [&prepared.transfer_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_path, "content://com.example/foto.jpg");
    }

    #[test]
    fn generated_archive_is_adopted_without_second_copy() {
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join("staging");
        let generated_dir = staging.join("zip-source");
        fs::create_dir_all(&generated_dir).unwrap();
        let generated = generated_dir.join("Nuvio-respaldo-parte-001-de-002.zip");
        let content = vec![9_u8; 512 * 1024];
        fs::write(&generated, &content).unwrap();

        let repository = CatalogRepository::open(&root.path().join("catalog.db")).unwrap();
        let prepared = TransferService::adopt_generated_upload_in_folder(
            &repository,
            generated.to_str().unwrap(),
            &staging,
            None,
        )
        .unwrap();

        assert_eq!(prepared.status, "ready");
        assert!(!generated.exists());
        let adopted = PathBuf::from(&prepared.local_path);
        assert!(adopted.exists());
        assert!(adopted.starts_with(&staging));
        assert_eq!(fs::read(&adopted).unwrap(), content);
        assert_eq!(
            prepared.sha256,
            crate::crypto::sha256_file(&adopted).unwrap()
        );
    }

    #[test]
    fn paused_preparation_can_resume_safely() {
        let root = std::env::temp_dir().join(format!("nuvio-pause-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source = root.join("large.bin");
        fs::write(&source, vec![7_u8; 2 * 1024 * 1024]).unwrap();
        let repository = CatalogRepository::open(&root.join("catalog.db")).unwrap();
        let staging = root.join("staging");
        let id = "transfer-paused";
        let size = fs::metadata(&source).unwrap().len() as i64;

        repository
            .create_upload_placeholder(id, "large.bin", source.to_str().unwrap(), size)
            .unwrap();
        repository
            .update_runtime(
                id,
                "analyzing",
                "analyzing",
                0,
                size,
                0,
                None,
                "Analizando",
                None,
            )
            .unwrap();
        repository.request_pause(id).unwrap();

        let paused = hash_with_progress(&repository, id, &source, size);
        assert!(paused.is_err());
        let job = repository
            .list_transfers()
            .unwrap()
            .into_iter()
            .find(|job| job.id == id)
            .unwrap();
        assert_eq!(job.status, "paused");
        assert!(repository.preparation_source(id).unwrap().is_some());

        let resumed = TransferService::resume_preparation(&repository, id, &staging).unwrap();
        assert_eq!(resumed.status, "ready");
        assert!(Path::new(&resumed.local_path).exists());
        assert!(repository.preparation_source(id).unwrap().is_none());
        let job = repository
            .list_transfers()
            .unwrap()
            .into_iter()
            .find(|job| job.id == id)
            .unwrap();
        assert_eq!(job.status, "ready");
        assert_eq!(job.processed_bytes, 0);

        let _ = fs::remove_dir_all(root);
    }
}
