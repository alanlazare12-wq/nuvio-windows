import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { onBackButtonPress } from "@tauri-apps/api/app";
import { open, save } from "@tauri-apps/plugin-dialog";
import type {
  AppSettings,
  BatchDownloadItem,
  CloudFile,
  DashboardData,
  DirectoryUploadPlan,
  DroppedPathInfo,
  MediaReady,
  PreparedUpload,
  TelegramAuthSnapshot,
} from "./types";

export function loadDashboard(): Promise<DashboardData> {
  return invoke<DashboardData>("get_dashboard");
}

export async function listenMobileBack(handler: () => void): Promise<() => void> {
  if (await invoke<string>("platform_name") !== "android") return () => {};
  const listener = await onBackButtonPress(handler);
  return () => { void listener.unregister(); };
}

export function backgroundApp(): Promise<void> { return invoke("background_app"); }

export type SyncNotificationData = {
  active: boolean;
  percent?: number | null;
  scanned: number;
  total?: number | null;
  phase?: string | null;
  error?: string | null;
};

export type UploadNotificationData = {
  active: boolean;
  total: number;
  completed: number;
  pending: number;
  failed: number;
  percent?: number | null;
  processedBytes: number;
  totalBytes: number;
  speedBps: number;
  currentFileName?: string | null;
};

export async function updateSyncNotification(data: SyncNotificationData): Promise<void> {
  try {
    if (await invoke<string>("platform_name") !== "android") return;
    await invoke("update_mobile_sync_notification", { data });
  } catch {
    // Notificaciones móviles opcionales
  }
}

export async function updateUploadNotification(data: UploadNotificationData): Promise<void> {
  try {
    if (await invoke<string>("platform_name") !== "android") return;
    await invoke("update_mobile_upload_notification", { data });
  } catch {
    // Notificaciones móviles opcionales
  }
}

export async function clearMobileNotification(id: number): Promise<void> {
  try {
    if (await invoke<string>("platform_name") !== "android") return;
    await invoke("clear_mobile_notification", { id });
  } catch {
    // Notificaciones móviles opcionales
  }
}


export async function setFavorite(id: string, favorite: boolean): Promise<void> {
  await invoke("set_favorite", { id, favorite });
}

export function syncFiles(): Promise<number> {
  return invoke("sync_files");
}

export function createFolder(name: string, parentId?: string | null): Promise<string> {
  return invoke<string>("create_folder", { name, parentId: parentId ?? null });
}

export function renameFolder(id: string, name: string): Promise<void> {
  return invoke("rename_folder", { id, name });
}

export function moveFolder(id: string, parentId?: string | null): Promise<void> {
  return invoke("move_folder", { id, parentId: parentId ?? null });
}

export function deleteFolder(id: string): Promise<void> {
  return invoke("delete_folder", { id });
}

export function moveFilesToFolder(ids: string[], folderId?: string | null): Promise<number> {
  return invoke<number>("move_files_to_folder", { ids, folderId: folderId ?? null });
}

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

export function setTrashed(id: string, trashed: boolean): Promise<void> {
  return invoke("set_trashed", { id, trashed });
}

export function setTrashedMany(ids: string[], trashed: boolean): Promise<number> {
  return invoke<number>("set_trashed_many", { ids, trashed });
}

export function deleteFilesPermanently(ids: string[]): Promise<number> {
  return invoke<number>("delete_files_permanently", { ids });
}

export function emptyTrash(): Promise<number> {
  return invoke<number>("empty_trash");
}

export async function selectDownloadDirectory(): Promise<string | null> {
  if (await invoke<string>("platform_name") === "android") {
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

export function prepareMedia(id: string): Promise<MediaReady> {
  return invoke<MediaReady>("prepare_media", { id });
}

export function clearMediaCache(): Promise<number> {
  return invoke<number>("clear_media_cache_command");
}

export function clearTransferHistory(): Promise<number> {
  return invoke<number>("clear_transfer_history");
}

export async function exportDiagnostics(): Promise<boolean> {
  const destination = await save({
    title: "Exportar diagnóstico de Nuvio",
    defaultPath: `nuvio-diagnostics-${new Date().toISOString().slice(0, 10)}.json`,
    filters: [{ name: "JSON", extensions: ["json"] }],
  });
  if (!destination) return false;
  await invoke("export_diagnostics", { destination });
  return true;
}

export async function downloadFile(file: CloudFile): Promise<boolean> {
  const directory = await selectDownloadDirectory();
  if (!directory) return false;
  const results = await queueDownloads([file.id], directory);
  return results.some((item) => item.status === "queued");
}

export async function selectFilesForUpload(): Promise<string[]> {
  if ((await invoke<string>("platform_name")) === "android") {
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
  if ((await invoke<string>("platform_name")) === "android") {
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

export async function inspectDroppedPaths(paths: string[]): Promise<DroppedPathInfo[]> {
  return invoke<DroppedPathInfo[]>("inspect_dropped_paths", { paths });
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

export async function decryptNuvioFile(
  inputPath: string,
  outputPath: string,
  passphrase: string,
): Promise<void> {
  await invoke("decrypt_nuvio_file", { inputPath, outputPath, passphrase });
}

export function updateSetting(key: keyof AppSettings | string, value: string): Promise<AppSettings> {
  return invoke<AppSettings>("update_setting", { key, value });
}

export function getTelegramAuthState(): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_auth_state");
}

export function configureTelegram(
  apiId: number,
  apiHash: string,
  rememberSession: boolean,
): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_configure", { apiId, apiHash, rememberSession });
}

export function submitTelegramPhone(phone: string): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_submit_phone", { phone });
}

export function submitTelegramEmail(email: string): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_submit_email", { email });
}

export function submitTelegramEmailCode(code: string): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_submit_email_code", { code });
}

export function submitTelegramCode(code: string): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_submit_code", { code });
}

export function resendTelegramCode(): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_resend_code");
}

export function submitTelegramPassword(password: string): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_submit_password", { password });
}

export function requestTelegramQr(): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_request_qr");
}

export function registerTelegramUser(
  firstName: string,
  lastName: string,
): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_register_user", { firstName, lastName });
}

export function logOutTelegram(): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_log_out");
}

export function forgetTelegramSession(): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_forget_session");
}

export function readableError(value: unknown): string {
  if (typeof value === "string" && value.trim()) return value;
  if (value instanceof Error && value.message.trim()) return value.message;
  try {
    const message = JSON.stringify(value);
    return message && message !== "null" && message !== '""' ? message : "Ocurrió un error inesperado";
  } catch {
    return "Ocurrió un error inesperado";
  }
}

export async function prepareZipUploads(items: { path: string; name?: string }[], folderId: string | null | undefined, onProgress: (processed: number, total: number, name: string) => void): Promise<PreparedUploadResult[]> {
  const unlisten = await listen<{ processedBytes: number; totalBytes: number; fileName: string }>("nuvio-zip-progress", event => onProgress(event.payload.processedBytes, event.payload.totalBytes, event.payload.fileName));
  try {
    const results = await invoke<({ Ok: PreparedUpload } | { Err: string })[]>("prepare_zip_uploads", { items, folderId: folderId ?? null });
    return results.map((result, index) => "Ok" in result ? { ok: true, path: result.Ok.fileName, upload: result.Ok } : { ok: false, path: `ZIP ${index + 1}`, error: result.Err });
  } finally { unlisten(); }
}
