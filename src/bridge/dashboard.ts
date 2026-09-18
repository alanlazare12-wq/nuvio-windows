import { invoke } from "@tauri-apps/api/core";
import type { CatalogPage, DashboardData, DashboardStatus, FileFilter, SectionKey, SortKey, SyncDelta } from "../types";

export function loadDashboard(): Promise<DashboardData> {
  return invoke<DashboardData>("get_dashboard");
}

export function loadDashboardStatus(afterHistoryCursor?: number | null): Promise<DashboardStatus> {
  return invoke<DashboardStatus>("get_dashboard_status", { afterHistoryCursor: afterHistoryCursor ?? null });
}

export type CatalogPageRequest = {
  section: Exclude<SectionKey, "history">;
  folderId?: string | null;
  kind?: FileFilter;
  search?: string;
  tag?: string | null;
  sort?: SortKey;
  offset?: number;
  limit?: number;
};

export function loadCatalogPage(request: CatalogPageRequest): Promise<CatalogPage> {
  return invoke<CatalogPage>("get_catalog_page", {
    section: request.section,
    folderId: request.folderId ?? null,
    kind: request.kind ?? "all",
    search: request.search ?? "",
    tag: request.tag ?? null,
    sort: request.sort ?? "recent",
    offset: request.offset ?? 0,
    limit: request.limit ?? 80,
  });
}

export function loadSyncDelta(afterCursor?: number | null): Promise<SyncDelta> {
  return invoke<SyncDelta>("get_sync_delta", { afterCursor: afterCursor ?? null });
}

export function syncFiles(): Promise<number> {
  return invoke("sync_files");
}
