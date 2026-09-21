use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

pub const ZIP_LIMIT: u64 = 2_000_000_000;
const SPLIT_MANIFEST_NAME: &str = "NUVIO-SPLIT-MANIFEST.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceProfile {
    Low,
    Balanced,
    Max,
}

impl ResourceProfile {
    pub fn from_setting(value: &str) -> Self {
        match value {
            "low" => Self::Low,
            "max" => Self::Max,
            _ => Self::Balanced,
        }
    }

    pub fn compression_concurrency(self) -> usize {
        match self {
            Self::Low | Self::Balanced => 1,
            Self::Max => 2,
        }
    }

    pub fn heavy_io_concurrency(self) -> usize {
        match self {
            Self::Low => 2,
            Self::Balanced => 4,
            Self::Max => 16,
        }
    }

    /// How much data may pass before the writer hands the CPU back. Every profile
    /// yields, including Max: measured on an NVMe host, a pass that never yields
    /// ran 4x *slower* (83-93 s vs 20-22 s for the same 4 GiB), because a writer
    /// that outruns the Windows lazy writer hits the dirty-page threshold and is
    /// forced into synchronous flushes. Pacing the stream is what keeps it fast.
    fn pause_after_bytes(self) -> u64 {
        match self {
            Self::Low => 2 * 1024 * 1024,
            Self::Balanced | Self::Max => 8 * 1024 * 1024,
        }
    }

    /// Balanced only yields the rest of its time slice. A real sleep is hostage to
    /// the Windows timer resolution — `Sleep(1)` can park the thread for a full
    /// 15.6 ms tick — which on a fast disk costs far more throughput than it buys
    /// in responsiveness now that the thread already runs below normal priority.
    /// Low keeps a genuine pause because that profile exists to leave the machine
    /// alone, and its cap is the point.
    fn pause_duration(self) -> Duration {
        match self {
            Self::Low => Duration::from_millis(4),
            Self::Balanced | Self::Max => Duration::ZERO,
        }
    }

    pub(crate) fn throttle(self, bytes_since_pause: &mut u64, processed: usize) {
        *bytes_since_pause = bytes_since_pause.saturating_add(processed as u64);
        if *bytes_since_pause < self.pause_after_bytes() {
            return;
        }
        *bytes_since_pause = 0;
        std::thread::yield_now();
        let pause = self.pause_duration();
        if !pause.is_zero() {
            std::thread::sleep(pause);
        }
    }
}

pub fn compression_concurrency(setting: &str) -> usize {
    ResourceProfile::from_setting(setting).compression_concurrency()
}

pub fn heavy_io_concurrency(setting: &str) -> usize {
    ResourceProfile::from_setting(setting).heavy_io_concurrency()
}

struct ResourceGovernor {
    profile: ResourceProfile,
    bytes_since_pause: u64,
}

impl ResourceGovernor {
    fn new(profile: ResourceProfile) -> Self {
        Self {
            profile,
            bytes_since_pause: 0,
        }
    }

    fn checkpoint(&mut self, processed: usize) {
        self.profile
            .throttle(&mut self.bytes_since_pause, processed);
    }
}

#[cfg(windows)]
struct ThreadPriorityGuard {
    previous: i32,
    changed_priority: bool,
}

#[cfg(windows)]
impl ThreadPriorityGuard {
    fn new(profile: ResourceProfile) -> Self {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
            THREAD_PRIORITY_LOWEST,
        };
        // Only CPU scheduling priority is lowered here, never Windows background mode
        // (THREAD_MODE_BACKGROUND_BEGIN). Background mode also drops the thread's I/O
        // priority to Very Low, a tier Windows reserves for the search indexer and
        // defragmenter and deliberately starves whenever anything else touches the
        // disk. Measured on an NVMe host it held a sequential split to 2.4 MB/s, which
        // turns a 90 GB backup into a ten-hour job.
        //
        // Every profile runs below normal, Max included, because on this workload a
        // lower priority is also the *faster* one: across three benchmark runs the
        // same 4 GiB split took 16-28 s below normal and 36-93 s at normal priority,
        // with no sample overlapping. A thread at normal priority holds the CPU
        // between I/O completions, starves the cache manager's flushers and ends up
        // stalling on synchronous writes. Max earns its name through
        // `compression_concurrency`, not by fighting the scheduler.
        unsafe {
            let thread = GetCurrentThread();
            let previous = GetThreadPriority(thread);
            let desired = if profile == ResourceProfile::Low {
                THREAD_PRIORITY_LOWEST
            } else {
                THREAD_PRIORITY_BELOW_NORMAL
            };
            let changed_priority = SetThreadPriority(thread, desired) != 0;
            Self {
                previous,
                changed_priority,
            }
        }
    }
}

#[cfg(windows)]
impl Drop for ThreadPriorityGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority};
        unsafe {
            if self.changed_priority {
                let _ = SetThreadPriority(GetCurrentThread(), self.previous);
            }
        }
    }
}

#[cfg(not(windows))]
struct ThreadPriorityGuard;

#[cfg(not(windows))]
impl ThreadPriorityGuard {
    fn new(_profile: ResourceProfile) -> Self {
        Self
    }
}

/// Opens a file for a long sequential pass. On Windows `FILE_FLAG_SEQUENTIAL_SCAN`
/// asks the cache manager to age these pages out as soon as they are consumed;
/// without it a 90 GB backup walks the whole system cache and evicts whatever the
/// user actually has open, which is what makes the desktop feel slow during a big
/// copy far more than CPU time does.
pub(crate) fn open_sequential(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
        File::options()
            .read(true)
            .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
            .open(path)
    }
    #[cfg(not(windows))]
    {
        File::open(path)
    }
}

pub(crate) fn with_resource_priority<T>(
    profile: ResourceProfile,
    operation: impl FnOnce() -> T,
) -> T {
    let _priority = ThreadPriorityGuard::new(profile);
    operation()
}

/// A finished archive ready to be adopted as an upload. `verified_sha256` carries
/// the hash of the file on disk when the verification pass already confirmed it,
/// so the upload preparation can skip re-reading every byte of a multi-gigabyte
/// volume just to compute what is already known.
pub struct ArchiveArtifact {
    pub path: PathBuf,
    pub verified_sha256: Option<String>,
}

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
            | "bz2"
            | "xz"
            | "zst"
            | "tgz"
            | "cab"
            | "jpg"
            | "jpeg"
            | "png"
            | "gif"
            | "webp"
            | "avif"
            | "jxl"
            | "heic"
            | "mp4"
            | "mkv"
            | "mov"
            | "m4v"
            | "webm"
            | "mp3"
            | "aac"
            | "m4a"
            | "flac"
            | "ogg"
            | "opus"
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
    // These archives are private staging artifacts that are immediately reopened and
    // verified before upload. Forcing FlushFileBuffers/sync_all after every multi-GB
    // part monopolizes the Windows storage queue and can freeze the desktop for seconds.
    // A normal flush is sufficient here; the verification pass remains the integrity gate.
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

/// Streams the finished volume once, hashing the whole `.zip` as it sits on disk
/// and, from the same bytes, the stored payload range. One sequential read yields
/// both the integrity check and the SHA-256 the uploader needs.
fn hash_volume_and_payload(
    path: &Path,
    payload_start: u64,
    payload_size: u64,
    processed: &mut u64,
    governor: &mut ResourceGovernor,
) -> Result<(String, String), String> {
    let mut reader = open_sequential(path).map_err(|error| error.to_string())?;
    let mut buffer = vec![0; 1024 * 1024];
    let mut file_hasher = Sha256::new();
    let mut payload_hasher = Sha256::new();
    let payload_end = payload_start
        .checked_add(payload_size)
        .ok_or("El volumen declara un contenido imposible")?;
    let mut offset = 0u64;
    let mut payload_seen = 0u64;
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        let chunk = &buffer[..count];
        file_hasher.update(chunk);
        let chunk_end = offset + count as u64;
        // Intersect this chunk with the payload range; a 1 MiB read can straddle
        // either edge of it.
        let from = payload_start.max(offset);
        let to = payload_end.min(chunk_end);
        if from < to {
            let start = (from - offset) as usize;
            let end = (to - offset) as usize;
            payload_hasher.update(&chunk[start..end]);
            payload_seen += (end - start) as u64;
        }
        offset = chunk_end;
        *processed = processed.saturating_add(count as u64);
        governor.checkpoint(count);
    }
    if payload_seen != payload_size {
        return Err(format!(
            "{} no contiene todo el volumen anunciado",
            path.display()
        ));
    }
    Ok((
        hex::encode(file_hasher.finalize()),
        hex::encode(payload_hasher.finalize()),
    ))
}

/// Fallback for a volume whose stored payload offset the ZIP reader did not
/// expose: verify through the entry itself, as before, and leave the uploader to
/// hash the file on its own.
fn hash_entry_payload(
    archive: &mut ZipArchive<File>,
    entry_name: &str,
    expected_size: u64,
    processed: &mut u64,
    governor: &mut ResourceGovernor,
) -> Result<String, String> {
    let mut entry = archive
        .by_name(entry_name)
        .map_err(|error| error.to_string())?;
    let mut buffer = vec![0; 1024 * 1024];
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
        governor.checkpoint(count);
    }
    if verified != expected_size {
        return Err("El volumen no contiene todo el contenido anunciado".into());
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Verifies every volume against what was actually written to disk and returns the
/// SHA-256 of each finished `.zip`, so the upload preparation does not have to read
/// all of it a second time.
fn verify_split_parts(
    source: &SourceFile,
    parts: &[SplitPart],
    original_sha256: &str,
    processed: &mut u64,
    total_work: u64,
    governor: &mut ResourceGovernor,
    report: &mut impl FnMut(ArchiveProgress),
) -> Result<Vec<Option<String>>, String> {
    let expected_count = u32::try_from(parts.len()).map_err(|_| "Demasiadas partes ZIP")?;
    let mut volume_hashes = Vec::with_capacity(parts.len());
    for part in parts {
        report(ArchiveProgress {
            processed_bytes: *processed,
            total_bytes: total_work,
            file_name: format!(
                "Verificando parte {:03}/{:03}",
                part.part_index, expected_count
            ),
        });
        let file = open_sequential(&part.path).map_err(|error| error.to_string())?;
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
        let payload_start = {
            let entry = archive
                .by_name(&part.entry_name)
                .map_err(|error| error.to_string())?;
            if entry.size() != part.size {
                return Err(format!(
                    "{} tiene un tamaño interno incorrecto",
                    part.path.display()
                ));
            }
            entry.data_start()
        };

        let (volume_hash, payload_hash) = match payload_start {
            Some(start) => {
                drop(archive);
                let (volume, payload) =
                    hash_volume_and_payload(&part.path, start, part.size, processed, governor)?;
                (Some(volume), payload)
            }
            None => {
                let payload = hash_entry_payload(
                    &mut archive,
                    &part.entry_name,
                    part.size,
                    processed,
                    governor,
                )?;
                (None, payload)
            }
        };
        if payload_hash != part.sha256 {
            return Err(format!(
                "{} no superó la verificación SHA-256",
                part.path.display()
            ));
        }
        volume_hashes.push(volume_hash);
    }
    Ok(volume_hashes)
}

struct ArchiveWriteState<'a, F: FnMut(ArchiveProgress)> {
    output: &'a Path,
    limit: u64,
    budget: u64,
    buffer: Vec<u8>,
    processed: u64,
    total_work: u64,
    last_report: Instant,
    governor: ResourceGovernor,
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
) -> Result<ArchiveArtifact, String> {
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
        let mut input = open_sequential(&source.path).map_err(|error| error.to_string())?;
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
            state.governor.checkpoint(count);
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
    Ok(ArchiveArtifact {
        path: target,
        verified_sha256: None,
    })
}

fn write_split_archives<F: FnMut(ArchiveProgress)>(
    source: &SourceFile,
    state: &mut ArchiveWriteState<'_, F>,
) -> Result<Vec<ArchiveArtifact>, String> {
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
    let mut input = open_sequential(&source.path).map_err(|error| error.to_string())?;
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
            state.governor.checkpoint(count);
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
    let volume_hashes = verify_split_parts(
        source,
        &parts,
        &original_sha256,
        &mut state.processed,
        state.total_work,
        &mut state.governor,
        &mut *state.report,
    )?;
    Ok(parts
        .into_iter()
        .zip(volume_hashes)
        .map(|(part, verified_sha256)| ArchiveArtifact {
            path: part.path,
            verified_sha256,
        })
        .collect())
}

#[cfg(test)]
pub fn create_archives(
    items: &[ArchiveInput],
    output: &Path,
    limit: u64,
    report: impl FnMut(ArchiveProgress),
) -> Result<Vec<ArchiveArtifact>, String> {
    create_archives_with_profile(items, output, limit, ResourceProfile::Balanced, report)
}

pub fn create_archives_with_profile(
    items: &[ArchiveInput],
    output: &Path,
    limit: u64,
    profile: ResourceProfile,
    mut report: impl FnMut(ArchiveProgress),
) -> Result<Vec<ArchiveArtifact>, String> {
    let _priority = ThreadPriorityGuard::new(profile);
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
        governor: ResourceGovernor::new(profile),
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

    #[test]
    fn resource_profiles_bound_compression_concurrency() {
        assert_eq!(
            ResourceProfile::from_setting("low").compression_concurrency(),
            1
        );
        assert_eq!(
            ResourceProfile::from_setting("balanced").compression_concurrency(),
            1
        );
        assert_eq!(
            ResourceProfile::from_setting("max").compression_concurrency(),
            2
        );
        assert_eq!(ResourceProfile::Low.heavy_io_concurrency(), 2);
        assert_eq!(ResourceProfile::Balanced.heavy_io_concurrency(), 4);
        assert_eq!(ResourceProfile::Max.heavy_io_concurrency(), 16);
        assert_eq!(
            ResourceProfile::from_setting("unexpected"),
            ResourceProfile::Balanced
        );
        assert!(stored("pelicula.mkv"));
        assert!(stored("audio.flac"));
        assert!(stored("respaldo.zst"));
        assert!(!stored("datos.csv"));
    }

    fn archive_paths(artifacts: &[ArchiveArtifact]) -> Vec<PathBuf> {
        artifacts.iter().map(|a| a.path.clone()).collect()
    }

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
        let artifacts = create_archives(
            &[ArchiveInput {
                path: source.to_string_lossy().into_owned(),
                name: Some("Carpeta/área.txt".into()),
            }],
            &root.path().join("out"),
            ZIP_LIMIT,
            |_| {},
        )
        .unwrap();
        let paths = archive_paths(&artifacts);
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

    /// Mide el coste real de dividir un archivo grande bajo cada perfil de
    /// recursos. `cargo test --release -- --ignored --nocapture split_throughput`
    #[test]
    #[ignore = "benchmark de E/S; ejecutar en release con --ignored"]
    fn split_throughput_by_resource_profile() {
        const SOURCE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
        const PART_LIMIT: u64 = 96 * 1024 * 1024;

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("respaldo.mkv");
        let mut writer =
            std::io::BufWriter::with_capacity(1024 * 1024, File::create(&source).unwrap());
        let block: Vec<u8> = (0..1024 * 1024).map(|index| (index % 251) as u8).collect();
        let mut written = 0u64;
        while written < SOURCE_BYTES {
            writer.write_all(&block).unwrap();
            written += block.len() as u64;
        }
        writer.into_inner().unwrap().sync_all().unwrap();
        println!("origen: {} MiB", SOURCE_BYTES / (1024 * 1024));

        // El orden se invierte en la segunda vuelta: leer el mismo origen dos
        // veces lo deja en la caché de Windows y favorecería al perfil que corra
        // segundo, así que cada uno corre en ambas posiciones.
        for profile in [
            ResourceProfile::Balanced,
            ResourceProfile::Max,
            ResourceProfile::Max,
            ResourceProfile::Balanced,
        ] {
            let out = root.path().join("out");
            fs::create_dir_all(&out).unwrap();
            let start = Instant::now();
            let parts = create_archives_with_profile(
                &[ArchiveInput {
                    path: source.to_string_lossy().into_owned(),
                    name: None,
                }],
                &out,
                PART_LIMIT,
                profile,
                |_| {},
            )
            .unwrap();
            let elapsed = start.elapsed();
            let mib = SOURCE_BYTES as f64 / (1024.0 * 1024.0);
            println!(
                "{profile:?}: {} partes en {:.1} s → {:.0} MiB/s de origen procesado",
                parts.len(),
                elapsed.as_secs_f64(),
                mib / elapsed.as_secs_f64()
            );
            fs::remove_dir_all(&out).unwrap();
        }
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
        let paths = archive_paths(
            &create_archives(&items, &root.path().join("out"), 10000, |_| {}).unwrap(),
        );
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
        let artifacts = create_archives(
            &[ArchiveInput {
                path: path.to_string_lossy().into_owned(),
                name: None,
            }],
            &root.path().join("out"),
            10000,
            |_| {},
        )
        .unwrap();
        let paths = archive_paths(&artifacts);
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
    fn split_volumes_carry_the_hash_of_the_file_on_disk() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("respaldo.rar");
        let original: Vec<u8> = (0..60000).map(|index| (index % 251) as u8).collect();
        fs::write(&path, &original).unwrap();
        let volumes = create_archives(
            &[ArchiveInput {
                path: path.to_string_lossy().into_owned(),
                name: None,
            }],
            &root.path().join("out"),
            10000,
            |_| {},
        )
        .unwrap();

        assert!(volumes.len() >= 6);
        for volume in &volumes {
            // The upload records this hash in the catalog without re-reading the
            // file, so it has to match the bytes that actually landed on disk.
            let declared = volume
                .verified_sha256
                .as_deref()
                .expect("cada volumen dividido trae su SHA-256 verificado");
            assert_eq!(declared, crate::crypto::sha256_file(&volume.path).unwrap());
        }

        // A regular archive is not verified byte by byte, so it must not claim a
        // hash the preparation would then trust blindly.
        let plain = root.path().join("nota.txt");
        fs::write(&plain, b"hola").unwrap();
        let regular = create_archives(
            &[ArchiveInput {
                path: plain.to_string_lossy().into_owned(),
                name: None,
            }],
            &root.path().join("out-regular"),
            ZIP_LIMIT,
            |_| {},
        )
        .unwrap();
        assert!(regular[0].verified_sha256.is_none());
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
        let paths = archive_paths(&paths);
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
