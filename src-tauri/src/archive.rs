use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

pub const ZIP_LIMIT: u64 = 2_000_000_000;
const SPLIT_MANIFEST_NAME: &str = "NUVIO-SPLIT-MANIFEST.json";

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

#[derive(Clone)]
struct SourceFile {
    path: PathBuf,
    name: String,
    size: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SplitManifest {
    format: String,
    original_name: String,
    original_size: u64,
    original_sha256: String,
    part_index: u32,
    part_count: u32,
    part_name: String,
    part_size: u64,
    part_sha256: String,
}

struct SplitPart {
    path: PathBuf,
    entry_name: String,
    part_index: u32,
    size: u64,
    sha256: String,
}

struct LimitedWriter {
    file: File,
    limit: u64,
}

impl Read for LimitedWriter {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.file.read(bytes)
    }
}

impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .file
            .stream_position()?
            .saturating_add(bytes.len() as u64)
            > self.limit
        {
            return Err(io::Error::other(
                "El ZIP supera el límite permitido de 2 GB",
            ));
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

fn output_label(name: &str) -> String {
    let normalized = name.replace('\\', "/");
    let candidate = Path::new(&normalized)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("respaldo");
    let mut label = String::with_capacity(candidate.len().min(96));
    for character in candidate.chars().take(96) {
        if character.is_alphanumeric() || matches!(character, '-' | '_' | '.') {
            label.push(character);
        } else if character.is_whitespace() {
            label.push('-');
        } else {
            label.push('_');
        }
    }
    let label = label.trim_matches(['.', '-', '_']).to_string();
    if label.is_empty() {
        "respaldo".into()
    } else {
        label
    }
}

fn finalize_zip(mut writer: LimitedWriter, target: &Path, limit: u64) -> Result<(), String> {
    writer.flush().map_err(|error| error.to_string())?;
    writer.file.sync_all().map_err(|error| error.to_string())?;
    if writer
        .file
        .metadata()
        .map_err(|error| error.to_string())?
        .len()
        > limit
    {
        return Err(format!("{} supera el límite permitido", target.display()));
    }
    Ok(())
}

fn verify_regular_archive(path: &Path, expected_entries: usize) -> Result<(), String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let archive = ZipArchive::new(file).map_err(|error| error.to_string())?;
    if archive.len() != expected_entries {
        return Err(format!(
            "{} no contiene todos los archivos esperados",
            path.display()
        ));
    }
    Ok(())
}

fn append_split_manifests(
    source: &SourceFile,
    parts: &[SplitPart],
    original_sha256: &str,
    limit: u64,
) -> Result<(), String> {
    let part_count = u32::try_from(parts.len()).map_err(|_| "Demasiadas partes ZIP")?;
    for part in parts {
        let manifest = SplitManifest {
            format: "nuvio-split-v1".into(),
            original_name: source.name.clone(),
            original_size: source.size,
            original_sha256: original_sha256.into(),
            part_index: part.part_index,
            part_count,
            part_name: part.entry_name.clone(),
            part_size: part.size,
            part_sha256: part.sha256.clone(),
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&part.path)
            .map_err(|error| error.to_string())?;
        let mut zip = ZipWriter::new_append(LimitedWriter { file, limit })
            .map_err(|error| error.to_string())?;
        zip.start_file(
            SPLIT_MANIFEST_NAME,
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .map_err(|error| error.to_string())?;
        zip.write_all(&manifest_bytes)
            .map_err(|error| error.to_string())?;
        let writer = zip.finish().map_err(|error| error.to_string())?;
        finalize_zip(writer, &part.path, limit)?;
    }
    Ok(())
}

fn verify_split_parts(
    source: &SourceFile,
    parts: &[SplitPart],
    original_sha256: &str,
    processed: &mut u64,
    total_work: u64,
    report: &mut impl FnMut(ArchiveProgress),
) -> Result<(), String> {
    let expected_count = u32::try_from(parts.len()).map_err(|_| "Demasiadas partes ZIP")?;
    let mut buffer = vec![0; 1024 * 1024];
    for part in parts {
        report(ArchiveProgress {
            processed_bytes: *processed,
            total_bytes: total_work,
            file_name: format!(
                "Verificando parte {:03}/{:03}",
                part.part_index, expected_count
            ),
        });
        let file = File::open(&part.path).map_err(|error| error.to_string())?;
        let mut archive = ZipArchive::new(file).map_err(|error| error.to_string())?;
        if archive.len() != 2 {
            return Err(format!(
                "{} no es un volumen Nuvio válido",
                part.path.display()
            ));
        }
        let manifest: SplitManifest = {
            let mut entry = archive
                .by_name(SPLIT_MANIFEST_NAME)
                .map_err(|error| error.to_string())?;
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?
        };
        if manifest.format != "nuvio-split-v1"
            || manifest.original_name != source.name
            || manifest.original_size != source.size
            || manifest.original_sha256 != original_sha256
            || manifest.part_index != part.part_index
            || manifest.part_count != expected_count
            || manifest.part_name != part.entry_name
            || manifest.part_size != part.size
            || manifest.part_sha256 != part.sha256
        {
            return Err(format!(
                "El manifiesto de {} no coincide con el archivo original",
                part.path.display()
            ));
        }
        let mut entry = archive
            .by_name(&part.entry_name)
            .map_err(|error| error.to_string())?;
        if entry.size() != part.size {
            return Err(format!(
                "{} tiene un tamaño interno incorrecto",
                part.path.display()
            ));
        }
        let mut hasher = Sha256::new();
        let mut verified = 0u64;
        loop {
            let count = entry.read(&mut buffer).map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            verified += count as u64;
            *processed = processed.saturating_add(count as u64);
        }
        if verified != part.size || hex::encode(hasher.finalize()) != part.sha256 {
            return Err(format!(
                "{} no superó la verificación SHA-256",
                part.path.display()
            ));
        }
    }
    Ok(())
}

struct ArchiveWriteState<'a, F: FnMut(ArchiveProgress)> {
    output: &'a Path,
    limit: u64,
    budget: u64,
    buffer: Vec<u8>,
    processed: u64,
    total_work: u64,
    last_report: Instant,
    report: &'a mut F,
}

impl<F: FnMut(ArchiveProgress)> ArchiveWriteState<'_, F> {
    fn maybe_report(&mut self, file_name: String) {
        if self.last_report.elapsed().as_millis() >= 200 {
            (self.report)(ArchiveProgress {
                processed_bytes: self.processed,
                total_bytes: self.total_work,
                file_name,
            });
            self.last_report = Instant::now();
        }
    }
}

fn write_regular_archive<F: FnMut(ArchiveProgress)>(
    sources: &[SourceFile],
    group: &[usize],
    sequence: usize,
    state: &mut ArchiveWriteState<'_, F>,
) -> Result<PathBuf, String> {
    let first_label = output_label(&sources[group[0]].name);
    let target = if group.len() == 1 {
        state
            .output
            .join(format!("Nuvio-{first_label}-lote-{sequence:03}.zip"))
    } else {
        state.output.join(format!(
            "Nuvio-{first_label}-y-{}-mas-lote-{sequence:03}.zip",
            group.len() - 1
        ))
    };
    let file = File::options()
        .write(true)
        .read(true)
        .create_new(true)
        .open(&target)
        .map_err(|error| error.to_string())?;
    let mut zip = ZipWriter::new(LimitedWriter {
        file,
        limit: state.limit,
    });
    for &index in group {
        let source = &sources[index];
        let method = if stored(&source.name) {
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
            .large_file(source.size >= u32::MAX as u64);
        zip.start_file(&source.name, options)
            .map_err(|error| error.to_string())?;
        let mut input = File::open(&source.path).map_err(|error| error.to_string())?;
        let mut read_size = 0u64;
        loop {
            let count = input
                .read(&mut state.buffer)
                .map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }
            zip.write_all(&state.buffer[..count])
                .map_err(|error| format!("{}: {error}", source.name))?;
            read_size += count as u64;
            state.processed = state.processed.saturating_add(count as u64);
            state.maybe_report(source.name.clone());
        }
        let current = input.metadata().map_err(|error| error.to_string())?;
        if read_size != source.size
            || current.len() != source.size
            || current.modified().ok() != source.modified
        {
            return Err(format!(
                "{} cambió durante la compresión; vuelve a seleccionarlo",
                source.name
            ));
        }
    }
    let writer = zip.finish().map_err(|error| error.to_string())?;
    finalize_zip(writer, &target, state.limit)?;
    verify_regular_archive(&target, group.len())?;
    Ok(target)
}

fn write_split_archives<F: FnMut(ArchiveProgress)>(
    source: &SourceFile,
    state: &mut ArchiveWriteState<'_, F>,
) -> Result<Vec<PathBuf>, String> {
    let reserve = (state.limit / 20).clamp(1024, 4 * 1024 * 1024);
    let chunk_limit = state
        .budget
        .checked_sub(reserve)
        .filter(|value| *value > 0)
        .ok_or("El límite ZIP es demasiado pequeño para crear volúmenes")?;
    let part_count_u64 = source.size.div_ceil(chunk_limit);
    let part_count =
        u32::try_from(part_count_u64).map_err(|_| "El archivo requiere demasiadas partes")?;
    let label = output_label(&source.name);
    let mut input = File::open(&source.path).map_err(|error| error.to_string())?;
    let mut global_hasher = Sha256::new();
    let mut parts = Vec::with_capacity(part_count as usize);
    let mut total_read = 0u64;

    for part_index in 1..=part_count {
        let target = state.output.join(format!(
            "Nuvio-{label}-parte-{part_index:03}-de-{part_count:03}.zip"
        ));
        let entry_name = format!(
            "{}.nuvio-part-{part_index:03}-of-{part_count:03}",
            source.name
        );
        let file = File::options()
            .write(true)
            .read(true)
            .create_new(true)
            .open(&target)
            .map_err(|error| error.to_string())?;
        let mut zip = ZipWriter::new(LimitedWriter {
            file,
            limit: state.limit,
        });
        zip.start_file(
            &entry_name,
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .map_err(|error| error.to_string())?;

        let mut remaining = source.size.saturating_sub(total_read).min(chunk_limit);
        let mut part_size = 0u64;
        let mut part_hasher = Sha256::new();
        while remaining > 0 {
            let wanted = usize::try_from(remaining.min(state.buffer.len() as u64))
                .unwrap_or(state.buffer.len());
            let count = input
                .read(&mut state.buffer[..wanted])
                .map_err(|error| error.to_string())?;
            if count == 0 {
                return Err(format!("{} terminó antes de lo esperado", source.name));
            }
            zip.write_all(&state.buffer[..count])
                .map_err(|error| format!("{}: {error}", source.name))?;
            global_hasher.update(&state.buffer[..count]);
            part_hasher.update(&state.buffer[..count]);
            part_size += count as u64;
            total_read += count as u64;
            remaining -= count as u64;
            state.processed = state.processed.saturating_add(count as u64);
            state.maybe_report(format!(
                "{} · parte {:03}/{:03}",
                source.name, part_index, part_count
            ));
        }
        let writer = zip.finish().map_err(|error| error.to_string())?;
        finalize_zip(writer, &target, state.limit)?;
        parts.push(SplitPart {
            path: target,
            entry_name,
            part_index,
            size: part_size,
            sha256: hex::encode(part_hasher.finalize()),
        });
    }

    let current = input.metadata().map_err(|error| error.to_string())?;
    if total_read != source.size
        || current.len() != source.size
        || current.modified().ok() != source.modified
    {
        return Err(format!(
            "{} cambió durante la división; vuelve a seleccionarlo",
            source.name
        ));
    }
    let original_sha256 = hex::encode(global_hasher.finalize());
    append_split_manifests(source, &parts, &original_sha256, state.limit)?;
    verify_split_parts(
        source,
        &parts,
        &original_sha256,
        &mut state.processed,
        state.total_work,
        &mut *state.report,
    )?;
    Ok(parts.into_iter().map(|part| part.path).collect())
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
    if limit < 4096 {
        return Err("El límite ZIP configurado es demasiado pequeño".into());
    }

    let mut sources = Vec::new();
    let mut paths = HashSet::new();
    let mut names = HashSet::new();
    let mut total = 0u64;
    for item in items {
        let path = crate::transfer::normalize_path(&item.path)?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if !paths.insert(path.clone()) {
            continue;
        }
        let meta = fs::metadata(&path).map_err(|error| error.to_string())?;
        if !meta.is_file() {
            return Err(
                "El ZIP solo admite archivos; selecciona la carpeta con Subir carpeta".into(),
            );
        }
        let requested = item
            .name
            .as_deref()
            .or_else(|| path.file_name().and_then(|name| name.to_str()))
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
        sources.push(SourceFile {
            path,
            name,
            size: meta.len(),
            modified: meta.modified().ok(),
        });
    }

    fs::create_dir_all(output).map_err(|error| error.to_string())?;
    let budget = limit / 100 * 95;
    if budget <= 4096 {
        return Err("El límite ZIP configurado no deja espacio útil".into());
    }

    enum Plan {
        Group(Vec<usize>),
        Split(usize),
    }
    let mut plans = Vec::new();
    let mut group = Vec::new();
    let mut group_size = 0u64;
    let mut split_total = 0u64;
    for (index, source) in sources.iter().enumerate() {
        let estimated = source.size.saturating_add(4096);
        if estimated > budget {
            if !group.is_empty() {
                plans.push(Plan::Group(std::mem::take(&mut group)));
                group_size = 0;
            }
            split_total = split_total
                .checked_add(source.size)
                .ok_or("Selección demasiado grande")?;
            plans.push(Plan::Split(index));
            continue;
        }
        if !group.is_empty() && group_size.saturating_add(estimated) > budget {
            plans.push(Plan::Group(std::mem::take(&mut group)));
            group_size = 0;
        }
        group.push(index);
        group_size = group_size.saturating_add(estimated);
    }
    if !group.is_empty() {
        plans.push(Plan::Group(group));
    }

    let total_work = total
        .checked_add(split_total)
        .ok_or("Selección demasiado grande")?;
    let mut output_paths = Vec::new();
    report(ArchiveProgress {
        processed_bytes: 0,
        total_bytes: total_work,
        file_name: "Preparando ZIP".into(),
    });
    let mut state = ArchiveWriteState {
        output,
        limit,
        budget,
        buffer: vec![0; 1024 * 1024],
        processed: 0,
        total_work,
        last_report: Instant::now(),
        report: &mut report,
    };

    let mut regular_sequence = 0usize;
    for plan in plans {
        match plan {
            Plan::Group(group) => {
                regular_sequence += 1;
                output_paths.push(write_regular_archive(
                    &sources,
                    &group,
                    regular_sequence,
                    &mut state,
                )?);
            }
            Plan::Split(index) => {
                output_paths.extend(write_split_archives(&sources[index], &mut state)?);
            }
        }
    }

    (state.report)(ArchiveProgress {
        processed_bytes: state.processed,
        total_bytes: state.total_work,
        file_name: "ZIP verificados y listos".into(),
    });
    Ok(output_paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_manifest(path: &Path) -> SplitManifest {
        let file = File::open(path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let mut entry = archive.by_name(SPLIT_MANIFEST_NAME).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

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
        let mut zip = ZipArchive::new(File::open(&paths[0]).unwrap()).unwrap();
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
    fn oversized_stored_file_is_split_verified_and_reconstructable() {
        let root = tempfile::tempdir().unwrap();
        let original: Vec<u8> = (0..25000).map(|index| (index % 251) as u8).collect();
        let path = root.path().join("archivo-grande.rar");
        fs::write(&path, &original).unwrap();
        let paths = create_archives(
            &[ArchiveInput {
                path: path.to_string_lossy().into_owned(),
                name: None,
            }],
            &root.path().join("out"),
            10000,
            |_| {},
        )
        .unwrap();
        assert!(paths.len() >= 3);
        assert!(paths
            .iter()
            .all(|part| fs::metadata(part).unwrap().len() <= 10000));

        let mut manifests: Vec<_> = paths
            .iter()
            .map(|part| (part.clone(), split_manifest(part)))
            .collect();
        manifests.sort_by_key(|(_, manifest)| manifest.part_index);
        let mut restored = Vec::new();
        for (part, manifest) in manifests {
            assert_eq!(manifest.original_name, "archivo-grande.rar");
            assert_eq!(manifest.original_size, original.len() as u64);
            assert_eq!(manifest.part_count as usize, paths.len());
            let file = File::open(part).unwrap();
            let mut archive = ZipArchive::new(file).unwrap();
            archive
                .by_name(&manifest.part_name)
                .unwrap()
                .read_to_end(&mut restored)
                .unwrap();
        }
        assert_eq!(restored, original);
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn split_parts_use_descriptive_names_and_manifests() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("respaldo importante.zip");
        fs::write(&path, vec![7; 22000]).unwrap();
        let paths = create_archives(
            &[ArchiveInput {
                path: path.to_string_lossy().into_owned(),
                name: None,
            }],
            &root.path().join("out"),
            10000,
            |_| {},
        )
        .unwrap();
        assert!(paths[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("respaldo-importante.zip-parte-001-de-"));
        let manifest = split_manifest(&paths[0]);
        assert_eq!(manifest.format, "nuvio-split-v1");
        assert_eq!(manifest.part_index, 1);
        assert_eq!(manifest.original_name, "respaldo importante.zip");
    }

    #[test]
    fn rejects_unsafe_archive_paths() {
        for name in ["../file", "/absolute", "C:/file", "x/../file", "a\nfile"] {
            assert!(safe_name(name).is_err());
        }
    }
}
