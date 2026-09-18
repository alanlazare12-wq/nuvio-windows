import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { BatchDownloadItem, CloudFile } from "../types";
import { getPlatform } from "./shared";

export function retryTransfer(id: string): Promise<void> {
  return invoke("retry_transfer", { id });
}

export function pauseTransfer(id: string): Promise<void> {
  return invoke("pause_transfer", { id });
}

export function cancelTransfer(id: string): Promise<void> {
  return invoke("cancel_transfer", { id });
}

export function resumeTransfer(id: string): Promise<void> {
  return invoke("resume_transfer", { id });
}

export function pauseQueue(): Promise<void> {
  return invoke("pause_queue");
}

export function resumeQueue(): Promise<void> {
  return invoke("resume_queue");
}

export async function selectDownloadDirectory(): Promise<string | null> {
  if ((await getPlatform()) === "android") {
    return invoke<string | null>("pick_download_directory");
  }
  const selected = await open({
    directory: true,
    multiple: false,
    title: "Elegir carpeta para las descargas de Nuvio",
  });
  return typeof selected === "string" ? selected : null;
}

export function queueDownloads(
  ids: string[],
  directory: string,
  conflictPolicy?: "skip" | "rename",
): Promise<BatchDownloadItem[]> {
  return invoke<BatchDownloadItem[]>("queue_downloads", {
    ids,
    directory,
    conflictPolicy: conflictPolicy ?? null,
  });
}

export function clearTransferHistory(): Promise<number> {
  return invoke<number>("clear_transfer_history");
}

export function deleteVerifiedUploadSource(id: string): Promise<boolean> {
  return invoke<boolean>("delete_verified_upload_source", { id });
}

export async function downloadFile(file: CloudFile): Promise<boolean> {
  const directory = await selectDownloadDirectory();
  if (!directory) return false;
  const results = await queueDownloads([file.id], directory);
  return results.some((item) => item.status === "queued");
}
