use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::Instant,
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

pub const ZIP_LIMIT: u64 = 2_000_000_000;
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveInput {
    pub path: String,
    pub name: Option<String>,
}
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveProgress {
    pub processed_bytes: u64,
    pub total_bytes: u64,
    pub file_name: String,
}

struct LimitedWriter {
    file: File,
    limit: u64,
}
impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .file
            .stream_position()?
            .saturating_add(bytes.len() as u64)
            > self.limit
        {
            return Err(io::Error::other("El ZIP supera 2 GB. Selecciona menos archivos o divide el archivo grande antes de comprimir."));
        }
        self.file.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl Seek for LimitedWriter {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}
fn safe_name(name: &str) -> Result<String, String> {
    let name = name.replace('\\', "/");
    if name.len() > 2000
        || name.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.contains(':')
                || part.chars().any(char::is_control)
        })
    {
        return Err("Nombre de archivo no válido dentro del ZIP".into());
    }
    Ok(name)
}
fn stored(name: &str) -> bool {
    matches!(
        Path::new(name)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "zip"
            | "rar"
            | "7z"
            | "gz"
            | "jpg"
            | "jpeg"
            | "png"
            | "webp"
            | "heic"
            | "mp4"
            | "mkv"
            | "mp3"
            | "aac"
            | "pdf"
            | "docx"
            | "xlsx"
            | "pptx"
            | "apk"
    )
}

pub fn create_archives(
    items: &[ArchiveInput],
    output: &Path,
    limit: u64,
    mut report: impl FnMut(ArchiveProgress),
) -> Result<Vec<PathBuf>, String> {
    if items.is_empty() || items.len() > 10000 {
        return Err("Selecciona entre 1 y 10000 archivos para comprimir".into());
    }
    let mut sources = Vec::new();
    let mut paths = HashSet::new();
    let mut names = HashSet::new();
    let mut total = 0u64;
    for item in items {
        let path = crate::transfer::normalize_path(&item.path)?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !paths.insert(path.clone()) {
            continue;
        }
        let meta = fs::metadata(&path).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err(
                "El ZIP solo admite archivos; selecciona la carpeta con Subir carpeta".into(),
            );
        }
        let requested = item
            .name
            .as_deref()
            .or_else(|| path.file_name().and_then(|n| n.to_str()))
            .ok_or("Nombre no válido")?;
        let original = safe_name(requested)?;
        let mut name = original.clone();
        let mut index = 2;
        while !names.insert(name.to_lowercase()) {
            name = format!("{index}-{original}");
            index += 1;
        }
        total = total
            .checked_add(meta.len())
            .ok_or("Selección demasiado grande")?;
        sources.push((path, name, meta.len(), meta.modified().ok()));
    }
    fs::create_dir_all(output).map_err(|e| e.to_string())?;
    let budget = limit / 100 * 95;
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut size = 0u64;
    for (index, source) in sources.iter().enumerate() {
        if groups.is_empty() || size.saturating_add(source.2).saturating_add(4096) > budget {
            groups.push(Vec::new());
            size = 0;
        }
        groups.last_mut().expect("archive group").push(index);
        size = size.saturating_add(source.2).saturating_add(4096);
    }
    let mut paths = Vec::new();
    let mut processed = 0u64;
    let mut buffer = vec![0; 1024 * 1024];
    let mut last_report = Instant::now();
    report(ArchiveProgress {
        processed_bytes: 0,
        total_bytes: total,
        file_name: "Preparando ZIP".into(),
    });
    for (group_index, group) in groups.iter().enumerate() {
        let target = output.join(format!("Nuvio-{:03}.zip", group_index + 1));
        let file = File::options()
            .write(true)
            .read(true)
            .create_new(true)
            .open(&target)
            .map_err(|e| e.to_string())?;
        let mut zip = ZipWriter::new(LimitedWriter { file, limit });
        for &index in group {
            let (source, name, size, modified) = &sources[index];
            let method = if stored(name) {
                CompressionMethod::Stored
            } else {
                CompressionMethod::Deflated
            };
            let options = SimpleFileOptions::default()
                .compression_method(method)
                .compression_level(if method == CompressionMethod::Stored {
                    None
                } else {
                    Some(1)
                })
                .large_file(*size >= u32::MAX as u64);
            zip.start_file(name, options).map_err(|e| e.to_string())?;
            let mut input = File::open(source).map_err(|e| e.to_string())?;
            let mut read_size = 0u64;
            loop {
                let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                zip.write_all(&buffer[..count])
                    .map_err(|e| format!("{name}: {e}"))?;
                read_size += count as u64;
                processed += count as u64;
                if last_report.elapsed().as_millis() >= 200 {
                    report(ArchiveProgress {
                        processed_bytes: processed,
                        total_bytes: total,
                        file_name: name.clone(),
                    });
                    last_report = Instant::now();
                }
            }
            let current = input.metadata().map_err(|e| e.to_string())?;
            if read_size != *size || current.len() != *size || current.modified().ok() != *modified
            {
                return Err(format!(
                    "{name} cambió durante la compresión; vuelve a seleccionarlo"
                ));
            }
        }
        let mut result = zip.finish().map_err(|e| e.to_string())?;
        result.flush().map_err(|e| e.to_string())?;
        if result.file.metadata().map_err(|e| e.to_string())?.len() > limit {
            return Err("El ZIP supera el límite permitido".into());
        }
        paths.push(target);
    }
    report(ArchiveProgress {
        processed_bytes: processed,
        total_bytes: total,
        file_name: "ZIP listos".into(),
    });
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zip_round_trip_compresses_and_preserves_directories() {
        let root = tempfile::tempdir().unwrap();
        let original = vec![b'x'; 100000];
        let source = root.path().join("original.txt");
        fs::write(&source, &original).unwrap();
        let paths = create_archives(
            &[ArchiveInput {
                path: source.to_string_lossy().into_owned(),
                name: Some("Carpeta/área.txt".into()),
            }],
            &root.path().join("out"),
            ZIP_LIMIT,
            |_| {},
        )
        .unwrap();
        assert!(fs::metadata(&paths[0]).unwrap().len() < 2000);
        let mut zip = zip::ZipArchive::new(File::open(&paths[0]).unwrap()).unwrap();
        let mut restored = Vec::new();
        zip.by_name("Carpeta/área.txt")
            .unwrap()
            .read_to_end(&mut restored)
            .unwrap();
        assert_eq!(restored, original);
        assert_eq!(fs::read(source).unwrap(), original);
    }
    #[test]
    fn packages_split_and_never_exceed_limit() {
        let root = tempfile::tempdir().unwrap();
        let mut items = Vec::new();
        for index in 0..3 {
            let path = root.path().join(format!("{index}.mp4"));
            fs::write(&path, vec![42; 6000]).unwrap();
            items.push(ArchiveInput {
                path: path.to_string_lossy().into_owned(),
                name: None,
            });
        }
        let paths = create_archives(&items, &root.path().join("out"), 10000, |_| {}).unwrap();
        assert_eq!(paths.len(), 3);
        for path in paths {
            assert!(fs::metadata(path).unwrap().len() <= 10000);
        }
    }
    #[test]
    fn oversized_stored_file_is_rejected_and_original_is_kept() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("video.mp4");
        fs::write(&path, vec![42; 20000]).unwrap();
        assert!(create_archives(
            &[ArchiveInput {
                path: path.to_string_lossy().into_owned(),
                name: None
            }],
            &root.path().join("out"),
            10000,
            |_| {}
        )
        .is_err());
        assert_eq!(fs::metadata(path).unwrap().len(), 20000);
    }
    #[test]
    fn rejects_unsafe_archive_paths() {
        for name in ["../file", "/absolute", "C:/file", "x/../file", "a\nfile"] {
            assert!(safe_name(name).is_err());
        }
    }
}
