use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::archive::ZIP_LIMIT;

pub const MANY_IMAGE_THRESHOLD: usize = 20;
pub const MANY_FILE_THRESHOLD: usize = 60;
pub const LARGE_FILE_COMPRESSION_THRESHOLD: u64 = 1_000_000_000;
pub const LARGE_SELECTION_THRESHOLD: u64 = ZIP_LIMIT;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadAdvisoryItem {
    pub path: String,
    pub name: Option<String>,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum UploadRecommendation {
    Direct,
    Compress,
    SplitVolumes,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UploadAdvisoryFile {
    pub name: String,
    pub size_bytes: u64,
    pub exceeds_telegram_limit: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UploadAdvisory {
    pub recommendation: UploadRecommendation,
    pub should_prompt: bool,
    pub file_count: usize,
    pub image_count: usize,
    pub total_bytes: u64,
    pub unknown_size_count: usize,
    pub oversized_count: usize,
    pub telegram_oversized_count: usize,
    pub telegram_limit_bytes: u64,
    pub largest_file_bytes: u64,
    pub oversized_files: Vec<UploadAdvisoryFile>,
    pub reason_codes: Vec<String>,
}

fn display_name(item: &UploadAdvisoryItem) -> String {
    item.name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            Path::new(&item.path)
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| item.path.clone())
}

fn is_image_name(name: &str) -> bool {
    let extension = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "jpg"
            | "jpeg"
            | "png"
            | "webp"
            | "heic"
            | "heif"
            | "gif"
            | "bmp"
            | "tif"
            | "tiff"
            | "avif"
            | "dng"
            | "raw"
            | "jxl"
    )
}

fn resolve_size(item: &UploadAdvisoryItem, name: &str) -> Result<Option<u64>, String> {
    if let Some(size) = item.size_bytes {
        return Ok(Some(size));
    }
    if item.path.starts_with("content://") {
        // Android content URIs are inspected after the platform grants/stages access.
        // Avoid copying a potentially huge document only to decide whether to prompt.
        return Ok(None);
    }
    let metadata = fs::metadata(&item.path)
        .map_err(|error| format!("No se pudo analizar \"{name}\": {error}"))?;
    if !metadata.is_file() {
        return Err(format!("\"{name}\" no es un archivo"));
    }
    Ok(Some(metadata.len()))
}

pub fn analyze_upload_selection(
    items: &[UploadAdvisoryItem],
    provider_limit_bytes: u64,
) -> Result<UploadAdvisory, String> {
    if items.is_empty() || items.len() > 10_000 {
        return Err("Selecciona entre 1 y 10000 archivos".into());
    }

    if provider_limit_bytes == 0 {
        return Err("El proveedor no informó un límite de subida válido".into());
    }
    let telegram_limit_bytes = provider_limit_bytes;

    let mut total_bytes = 0u64;
    let mut image_count = 0usize;
    let mut unknown_size_count = 0usize;
    let mut oversized_count = 0usize;
    let mut telegram_oversized_count = 0usize;
    let mut largest_file_bytes = 0u64;
    let mut oversized_files = Vec::new();

    for item in items {
        if item.path.trim().is_empty() {
            return Err("La selección contiene una ruta vacía".into());
        }
        let name = display_name(item);
        if is_image_name(&name) {
            image_count += 1;
        }

        let Some(size) = resolve_size(item, &name)? else {
            unknown_size_count += 1;
            continue;
        };

        total_bytes = total_bytes.saturating_add(size);
        largest_file_bytes = largest_file_bytes.max(size);

        let exceeds_telegram_limit = size > telegram_limit_bytes;
        if exceeds_telegram_limit {
            telegram_oversized_count += 1;
        }

        if size > ZIP_LIMIT {
            oversized_count += 1;
            if oversized_files.len() < 8 {
                oversized_files.push(UploadAdvisoryFile {
                    name,
                    size_bytes: size,
                    exceeds_telegram_limit,
                });
            }
        }
    }

    let many_images = image_count >= MANY_IMAGE_THRESHOLD;
    let many_files = items.len() >= MANY_FILE_THRESHOLD;
    let large_single_file =
        items.len() == 1 && largest_file_bytes > LARGE_FILE_COMPRESSION_THRESHOLD;
    let large_multi_selection = items.len() > 1 && total_bytes > LARGE_SELECTION_THRESHOLD;

    let recommendation = if oversized_count > 0 {
        UploadRecommendation::SplitVolumes
    } else if large_single_file || many_images || many_files || large_multi_selection {
        UploadRecommendation::Compress
    } else {
        UploadRecommendation::Direct
    };

    let mut reason_codes = Vec::new();
    if oversized_count > 0 {
        reason_codes.push("fileOver2Gb".to_string());
    }
    if large_single_file {
        reason_codes.push("largeFile".to_string());
    }
    if many_images {
        reason_codes.push("manyImages".to_string());
    }
    if many_files {
        reason_codes.push("manyFiles".to_string());
    }
    if large_multi_selection {
        reason_codes.push("selectionOver2Gb".to_string());
    }
    if telegram_oversized_count > 0 {
        reason_codes.push("telegramLimitExceeded".to_string());
    }
    if unknown_size_count > 0 {
        reason_codes.push("unknownSizes".to_string());
    }

    Ok(UploadAdvisory {
        should_prompt: recommendation != UploadRecommendation::Direct,
        recommendation,
        file_count: items.len(),
        image_count,
        total_bytes,
        unknown_size_count,
        oversized_count,
        telegram_oversized_count,
        telegram_limit_bytes,
        largest_file_bytes,
        oversized_files,
        reason_codes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STANDARD_LIMIT: u64 = 2_000_000_000;
    const PREMIUM_LIMIT: u64 = 4_000_000_000;

    fn item(name: &str, size_bytes: u64) -> UploadAdvisoryItem {
        UploadAdvisoryItem {
            path: format!("C:/QA/{name}"),
            name: Some(name.to_string()),
            size_bytes: Some(size_bytes),
        }
    }

    #[test]
    fn oversized_file_recommends_split_even_for_premium() {
        let report =
            analyze_upload_selection(&[item("respaldo.rar", 2_500_000_000)], PREMIUM_LIMIT)
                .unwrap();

        assert_eq!(report.recommendation, UploadRecommendation::SplitVolumes);
        assert!(report.should_prompt);
        assert_eq!(report.oversized_count, 1);
        assert_eq!(report.telegram_oversized_count, 0);
    }

    #[test]
    fn large_single_file_recommends_compression_before_split_threshold() {
        let report =
            analyze_upload_selection(&[item("video.mkv", 1_500_000_000)], STANDARD_LIMIT).unwrap();

        assert_eq!(report.recommendation, UploadRecommendation::Compress);
        assert_eq!(report.oversized_count, 0);
        assert!(report
            .reason_codes
            .iter()
            .any(|reason| reason == "largeFile"));
    }

    #[test]
    fn many_images_recommend_compression() {
        let items: Vec<_> = (0..MANY_IMAGE_THRESHOLD)
            .map(|index| item(&format!("foto-{index}.jpg"), 5_000_000))
            .collect();
        let report = analyze_upload_selection(&items, STANDARD_LIMIT).unwrap();

        assert_eq!(report.recommendation, UploadRecommendation::Compress);
        assert_eq!(report.image_count, MANY_IMAGE_THRESHOLD);
        assert!(report
            .reason_codes
            .iter()
            .any(|reason| reason == "manyImages"));
    }

    #[test]
    fn large_multi_file_selection_recommends_compression() {
        let report = analyze_upload_selection(
            &[item("a.bin", 1_200_000_000), item("b.bin", 1_100_000_000)],
            STANDARD_LIMIT,
        )
        .unwrap();

        assert_eq!(report.recommendation, UploadRecommendation::Compress);
        assert!(report
            .reason_codes
            .iter()
            .any(|reason| reason == "selectionOver2Gb"));
    }

    #[test]
    fn small_selection_stays_direct() {
        let report = analyze_upload_selection(
            &[item("uno.txt", 1024), item("dos.txt", 2048)],
            STANDARD_LIMIT,
        )
        .unwrap();

        assert_eq!(report.recommendation, UploadRecommendation::Direct);
        assert!(!report.should_prompt);
    }

    #[test]
    fn telegram_limit_is_reported_separately_from_split_threshold() {
        let report =
            analyze_upload_selection(&[item("video.mkv", 3_500_000_000)], STANDARD_LIMIT).unwrap();

        assert_eq!(report.recommendation, UploadRecommendation::SplitVolumes);
        assert_eq!(report.telegram_oversized_count, 1);
        assert!(report.oversized_files[0].exceeds_telegram_limit);
    }
}
