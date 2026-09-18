import { invoke } from "@tauri-apps/api/core";
import type {
  MediaReady,
  UploadedImageCleanupResult,
  UploadedImageCleanupSummary,
} from "../types";

export function prepareMedia(id: string): Promise<MediaReady> {
  return invoke<MediaReady>("prepare_media", { id });
}

export function clearMediaCache(): Promise<number> {
  return invoke<number>("clear_media_cache_command");
}

export function getUploadedImageCleanupSummary(): Promise<UploadedImageCleanupSummary> {
  return invoke<UploadedImageCleanupSummary>("uploaded_image_cleanup_summary");
}

export function deleteUploadedImageSources(): Promise<UploadedImageCleanupResult> {
  return invoke<UploadedImageCleanupResult>("delete_uploaded_image_sources");
}
