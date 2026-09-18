import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  DirectoryUploadPlan,
  DroppedPathInfo,
  PreparedUpload,
  UploadAdvisory,
  UploadAdvisoryInput,
} from "../types";
import { getPlatform, readableError } from "./shared";

export async function selectFilesForUpload(): Promise<string[]> {
  if ((await getPlatform()) === "android") {
    return invoke<string[]>("pick_upload_files");
  }
  const selected = await open({
    multiple: true,
    directory: false,
    fileAccessMode: "copy",
    title: "Seleccionar archivos para Nuvio",
  });
  if (!selected) return [];
  return Array.isArray(selected) ? selected : [selected];
}

export async function selectFolderForUpload(): Promise<DirectoryUploadPlan | null> {
  if ((await getPlatform()) === "android") {
    return invoke<DirectoryUploadPlan | null>("pick_upload_directory");
  }
  const selected = await open({
    multiple: false,
    directory: true,
    title: "Seleccionar carpeta para subir a Nuvio",
  });
  if (!selected || typeof selected !== "string") return null;
  return scanDirectoryForUpload(selected);
}

export function scanDirectoryForUpload(path: string): Promise<DirectoryUploadPlan> {
  return invoke<DirectoryUploadPlan>("scan_directory_for_upload", { path });
}

export function prepareUpload(
  path: string,
  encrypt = false,
  passphrase?: string,
  folderId?: string | null,
): Promise<PreparedUpload> {
  return invoke<PreparedUpload>("prepare_upload", {
    path,
    encrypt,
    passphrase: passphrase ?? null,
    folderId: folderId ?? null,
  });
}

export type PreparedUploadResult =
  | { ok: true; path: string; upload: PreparedUpload }
  | { ok: false; path: string; error: string };

export type UploadQueueItem = {
  path: string;
  folderId?: string | null;
};

export function inspectDroppedPaths(paths: string[]): Promise<DroppedPathInfo[]> {
  return invoke<DroppedPathInfo[]>("inspect_dropped_paths", { paths });
}

export function analyzeUploadSelection(items: UploadAdvisoryInput[]): Promise<UploadAdvisory> {
  return invoke<UploadAdvisory>("analyze_upload_selection", { items });
}

export async function prepareUploadItemsBatch(
  items: UploadQueueItem[],
  concurrency = 4,
  onSettled?: (result: PreparedUploadResult, completed: number, total: number) => void,
): Promise<PreparedUploadResult[]> {
  const results = new Array<PreparedUploadResult>(items.length);
  let cursor = 0;
  let completed = 0;
  const workerCount = Math.max(1, Math.min(concurrency, items.length));
  const workers = Array.from({ length: workerCount }, async () => {
    while (true) {
      const index = cursor++;
      if (index >= items.length) return;
      const item = items[index];
      try {
        results[index] = {
          ok: true,
          path: item.path,
          upload: await prepareUpload(item.path, false, undefined, item.folderId),
        };
      } catch (error) {
        results[index] = { ok: false, path: item.path, error: readableError(error) };
      }
      completed += 1;
      onSettled?.(results[index], completed, items.length);
    }
  });
  await Promise.all(workers);
  return results;
}

export function prepareUploadBatch(
  paths: string[],
  concurrency = 4,
  onSettled?: (result: PreparedUploadResult, completed: number, total: number) => void,
  folderId?: string | null,
): Promise<PreparedUploadResult[]> {
  return prepareUploadItemsBatch(
    paths.map((path) => ({ path, folderId })),
    concurrency,
    onSettled,
  );
}

export async function prepareZipUploads(
  items: { path: string; name?: string }[],
  folderId: string | null | undefined,
  onProgress: (processed: number, total: number, name: string) => void,
): Promise<PreparedUploadResult[]> {
  const unlisten = await listen<{ processedBytes: number; totalBytes: number; fileName: string }>(
    "nuvio-zip-progress",
    (event) => onProgress(
      event.payload.processedBytes,
      event.payload.totalBytes,
      event.payload.fileName,
    ),
  );
  try {
    const results = await invoke<({ Ok: PreparedUpload } | { Err: string })[]>(
      "prepare_zip_uploads",
      { items, folderId: folderId ?? null },
    );
    return results.map((result, index) =>
      "Ok" in result
        ? { ok: true, path: result.Ok.fileName, upload: result.Ok }
        : { ok: false, path: `ZIP ${index + 1}`, error: result.Err },
    );
  } finally {
    unlisten();
  }
}

export async function decryptNuvioFile(
  inputPath: string,
  outputPath: string,
  passphrase: string,
): Promise<void> {
  await invoke("decrypt_nuvio_file", { inputPath, outputPath, passphrase });
}
