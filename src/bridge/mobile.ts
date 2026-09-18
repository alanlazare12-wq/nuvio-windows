import { invoke } from "@tauri-apps/api/core";
import { onBackButtonPress } from "@tauri-apps/api/app";
import type { SyncPhase } from "../types";
import { getPlatform } from "./shared";

export async function listenMobileBack(handler: () => void): Promise<() => void> {
  if ((await getPlatform()) !== "android") return () => {};
  const listener = await onBackButtonPress(handler);
  return () => { void listener.unregister(); };
}

export function backgroundApp(): Promise<void> {
  return invoke("background_app");
}

export type SyncNotificationData = {
  active: boolean;
  percent?: number | null;
  scanned: number;
  total?: number | null;
  phase?: SyncPhase | null;
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
    if ((await getPlatform()) !== "android") return;
    await invoke("update_mobile_sync_notification", { data });
  } catch {
    // Notificaciones móviles opcionales.
  }
}

export async function updateUploadNotification(data: UploadNotificationData): Promise<void> {
  try {
    if ((await getPlatform()) !== "android") return;
    await invoke("update_mobile_upload_notification", { data });
  } catch {
    // Notificaciones móviles opcionales.
  }
}

export async function clearMobileNotification(id: number): Promise<void> {
  try {
    if ((await getPlatform()) !== "android") return;
    await invoke("clear_mobile_notification", { id });
  } catch {
    // Notificaciones móviles opcionales.
  }
}
