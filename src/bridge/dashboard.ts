import { invoke } from "@tauri-apps/api/core";
import type { DashboardData, SyncDelta } from "../types";

export function loadDashboard(): Promise<DashboardData> {
  return invoke<DashboardData>("get_dashboard");
}

export function loadSyncDelta(afterRowid?: number | null): Promise<SyncDelta> {
  return invoke<SyncDelta>("get_sync_delta", { afterRowid: afterRowid ?? null });
}

export function syncFiles(): Promise<number> {
  return invoke("sync_files");
}
