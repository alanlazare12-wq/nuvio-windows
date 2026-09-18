use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use tdlib_rs::{enums as e, functions as f};

use crate::cloud::verified_copy;
use crate::crypto::sha256_file;
use crate::repository::CatalogRepository;
use crate::telegram::{call, TelegramService};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaReady {
    pub file_id: String,
    pub path: String,
    pub size_bytes: i64,
    pub from_cache: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailSource {
    pub kind: String,
    pub path: Option<String>,
    pub data_url: Option<String>,
    pub blurred: bool,
}

impl TelegramService {
    pub async fn prepare_thumbnail(
        &self,
        repo: &CatalogRepository,
        file_id: &str,
        cache_dir: &Path,
        cache_limit: i64,
    ) -> Result<Option<ThumbnailSource>, String> {
        let doc = repo.remote(file_id)?;
        if let Some(mini) = doc
            .minithumbnail
            .as_ref()
            .filter(|data| !data.is_empty() && data.len() <= 64 * 1024)
        {
            return Ok(Some(ThumbnailSource {
                kind: "image".into(),
                path: None,
                data_url: Some(format!("data:image/jpeg;base64,{mini}")),
                blurred: true,
            }));
        }
        let chat = self.own_chat(repo).await?;
        let e::Message::Message(message) =
            call(f::get_message(chat, doc.message_id, self.client_id())).await?;
        let e::MessageContent::MessageDocument(content) = message.content else {
            return Ok(None);
        };
        // Telegram's tiny JPEG is already in the message: no original download is needed.
        if let Some(mini) = content
            .document
            .minithumbnail
            .filter(|mini| mini.data.len() <= 64 * 1024)
        {
            repo.cache_minithumbnail(file_id, &mini.data)?;
            return Ok(Some(ThumbnailSource {
                kind: "image".into(),
                path: None,
                data_url: Some(format!("data:image/jpeg;base64,{}", mini.data)),
                blurred: true,
            }));
        }
        if let Some(thumb) = content
            .document
            .thumbnail
            .filter(|thumb| thumb.file.size > 0 && thumb.file.size <= 1024 * 1024)
        {
            let e::File::File(file) = call(f::download_file(
                thumb.file.id,
                1,
                0,
                0,
                true,
                self.client_id(),
            ))
            .await?;
            if file.local.is_downloading_completed {
                let source = Path::new(&file.local.path);
                let size = fs::metadata(source).map_err(|e| e.to_string())?.len();
                if size > 0 && size <= 1024 * 1024 {
                    fs::create_dir_all(cache_dir).map_err(|e| e.to_string())?;
                    let target = cache_dir.join(format!("{}.thumb.jpg", sanitize_id(file_id)));
                    let hash = sha256_file(source).map_err(|e| e.to_string())?;
                    copy_media(source, &target, &hash, size as i64)?;
                    clean_cache(cache_dir, cache_limit, Some(&target))?;
                    return Ok(Some(ThumbnailSource {
                        kind: "image".into(),
                        path: Some(target.to_string_lossy().into_owned()),
                        data_url: None,
                        blurred: false,
                    }));
                }
            }
        }
        let extension = Path::new(&doc.name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(kind) = original_thumbnail_kind(&extension, doc.size) else {
            return Ok(None);
        };
        let ready = self
            .prepare_media(repo, file_id, cache_dir, cache_limit)
            .await?;
        Ok(Some(ThumbnailSource {
            kind: kind.into(),
            path: Some(ready.path),
            data_url: None,
            blurred: false,
        }))
    }

    pub async fn prepare_media(
        &self,
        repo: &CatalogRepository,
        file_id: &str,
        cache_dir: &Path,
        cache_limit: i64,
    ) -> Result<MediaReady, String> {
        let doc = repo.remote(file_id)?;
        fs::create_dir_all(cache_dir).map_err(|e| e.to_string())?;
        let extension = Path::new(&doc.name)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("bin");
        let cache_path = cache_dir.join(format!("{}.{extension}", sanitize_id(file_id)));

        if cache_path.exists()
            && fs::metadata(&cache_path).map_err(|e| e.to_string())?.len() == doc.size as u64
            && sha256_file(&cache_path).map_err(|e| e.to_string())? == doc.sha256
        {
            clean_cache(cache_dir, cache_limit, Some(&cache_path))?;
            return Ok(MediaReady {
                file_id: file_id.to_string(),
                path: cache_path.to_string_lossy().into_owned(),
                size_bytes: doc.size,
                from_cache: true,
            });
        }
        let _ = fs::remove_file(&cache_path);

        let chat = self.own_chat(repo).await?;
        let e::Message::Message(message) =
            call(f::get_message(chat, doc.message_id, self.client_id())).await?;
        let e::MessageContent::MessageDocument(content) = message.content else {
            return Err("Este archivo no se puede previsualizar como contenido de Nuvio".into());
        };
        let td_file_id = content.document.document.id;

        // Request an initial prefix first, then request the whole file. This avoids a separate
        // user-visible download and lets TDLib reuse whatever prefix is already cached locally.
        let prefix = doc.size.clamp(1, 8 * 1024 * 1024);
        call(f::download_file(
            td_file_id,
            32,
            0,
            prefix,
            false,
            self.client_id(),
        ))
        .await?;
        let prefix_deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < prefix_deadline {
            let e::File::File(file) = call(f::get_file(td_file_id, self.client_id())).await?;
            if file.local.is_downloading_completed || file.local.downloaded_prefix_size >= prefix {
                break;
            }
            tokio::time::sleep(Duration::from_millis(180)).await;
        }
        call(f::download_file(
            td_file_id,
            32,
            0,
            0,
            false,
            self.client_id(),
        ))
        .await?;

        let deadline = Instant::now() + Duration::from_secs(3600);
        while Instant::now() < deadline {
            let e::File::File(file) = call(f::get_file(td_file_id, self.client_id())).await?;
            if file.local.is_downloading_completed {
                let source = PathBuf::from(file.local.path);
                let from_cache = copy_media(&source, &cache_path, &doc.sha256, doc.size)?;
                clean_cache(cache_dir, cache_limit, Some(&cache_path))?;
                return Ok(MediaReady {
                    file_id: file_id.to_string(),
                    path: cache_path.to_string_lossy().into_owned(),
                    size_bytes: doc.size,
                    from_cache,
                });
            }
            if !file.local.is_downloading_active {
                return Err("Telegram interrumpió la preparación de la vista previa".into());
            }
            tokio::time::sleep(Duration::from_millis(350)).await;
        }
        Err("La preparación de la vista previa tardó demasiado".into())
    }
}

fn original_thumbnail_kind(extension: &str, size: i64) -> Option<&'static str> {
    if !(1..=8 * 1024 * 1024).contains(&size) {
        return None;
    }
    match extension {
        "jpg" | "jpeg" | "png" | "webp" | "gif" => Some("image"),
        "pdf" => Some("pdf"),
        "txt" | "md" | "json" | "csv" | "rs" | "js" | "ts" | "css" | "html" | "py" | "xml"
        | "yaml" | "yml"
            if size <= 256 * 1024 =>
        {
            Some("text")
        }
        _ => None,
    }
}

#[test]
fn thumbnails_never_download_large_or_unsupported_originals() {
    assert_eq!(
        original_thumbnail_kind("jpg", 8 * 1024 * 1024),
        Some("image")
    );
    assert_eq!(original_thumbnail_kind("jpg", 8 * 1024 * 1024 + 1), None);
    assert_eq!(original_thumbnail_kind("mp4", 1024), None);
    assert_eq!(original_thumbnail_kind("pdf", -1), None);
    assert_eq!(original_thumbnail_kind("txt", 256 * 1024 + 1), None);
}

fn copy_media(source: &Path, target: &Path, hash: &str, size: i64) -> Result<bool, String> {
    match verified_copy(source, target, hash, size) {
        Ok(()) => Ok(false),
        Err(error) => {
            // Reopening a preview can finish two preparations of the same file.
            // Accept the other verified copy without deleting or overwriting it.
            let valid = fs::metadata(target).is_ok_and(|metadata| metadata.len() == size as u64)
                && sha256_file(target).is_ok_and(|actual| actual == hash);
            if valid {
                Ok(true)
            } else {
                Err(error)
            }
        }
    }
}

pub fn remove_media_cache_entries(cache_dir: &Path, file_ids: &[String]) -> Result<u64, String> {
    if !cache_dir.exists() || file_ids.is_empty() {
        return Ok(0);
    }
    let prefixes: Vec<String> = file_ids
        .iter()
        .map(|id| format!("{}.", sanitize_id(id)))
        .collect();
    let mut removed = 0_u64;
    for entry in fs::read_dir(cache_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let metadata = entry.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if prefixes.iter().any(|prefix| name.starts_with(prefix)) {
            fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
            removed = removed.saturating_add(metadata.len());
        }
    }
    Ok(removed)
}

pub fn clear_media_cache(cache_dir: &Path) -> Result<u64, String> {
    if !cache_dir.exists() {
        return Ok(0);
    }
    let before = directory_size(cache_dir)?;
    for entry in fs::read_dir(cache_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_file() {
            fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
        }
    }
    Ok(before)
}

fn clean_cache(cache_dir: &Path, limit: i64, keep: Option<&Path>) -> Result<(), String> {
    let limit = limit.max(0) as u64;
    let mut entries = Vec::new();
    let mut total = 0_u64;
    for entry in fs::read_dir(cache_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let metadata = entry.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            continue;
        }
        total = total.saturating_add(metadata.len());
        entries.push((
            entry.path(),
            metadata.len(),
            metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        ));
    }
    entries.sort_by_key(|entry| entry.2);
    for (path, size, _) in entries {
        if total <= limit {
            break;
        }
        if keep.is_some_and(|keep_path| keep_path == path) {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
    Ok(())
}

fn directory_size(path: &Path) -> Result<u64, String> {
    let mut total = 0_u64;
    if !path.exists() {
        return Ok(0);
    }
    for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let metadata = entry.metadata().map_err(|e| e.to_string())?;
        total = total.saturating_add(if metadata.is_dir() {
            directory_size(&entry.path())?
        } else {
            metadata.len()
        });
    }
    Ok(total)
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .take(120)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simultaneous_previews_reuse_the_verified_cache_file() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.wav");
        let target = root.path().join("cache.wav");
        fs::write(&source, b"audio").unwrap();
        let hash = sha256_file(&source).unwrap();
        std::thread::scope(|scope| {
            let first = scope.spawn(|| copy_media(&source, &target, &hash, 5).unwrap());
            let second = scope.spawn(|| copy_media(&source, &target, &hash, 5).unwrap());
            assert_ne!(first.join().unwrap(), second.join().unwrap());
        });
        assert_eq!(fs::read(&target).unwrap(), b"audio");
        fs::write(&target, b"wrong").unwrap();
        assert!(copy_media(&source, &target, &hash, 5).is_err());
    }

    #[test]
    fn sanitizer_does_not_allow_path_traversal() {
        assert_eq!(sanitize_id("../../tg:12"), "______tg_12");
    }

    #[test]
    fn cache_cleanup_keeps_requested_file() {
        let root = tempfile::tempdir().unwrap();
        let keep = root.path().join("keep.mp4");
        let old = root.path().join("old.mp4");
        fs::write(&old, vec![0_u8; 1024]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&keep, vec![0_u8; 1024]).unwrap();
        clean_cache(root.path(), 1024, Some(&keep)).unwrap();
        assert!(keep.exists());
        assert!(!old.exists());
    }
}
