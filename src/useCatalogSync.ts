import {
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";
import { loadSyncDelta, syncFiles } from "./bridge/dashboard";
import { readableError } from "./bridge/shared";
import type { CloudFile, DashboardData } from "./types";

type UseCatalogSyncOptions = {
  dashboard: DashboardData | null;
  setDashboard: Dispatch<SetStateAction<DashboardData | null>>;
  refreshDashboard: () => Promise<DashboardData | undefined>;
  setNotice: Dispatch<SetStateAction<string | null>>;
};

function mergeRecentFiles(current: CloudFile[], added: CloudFile[]): CloudFile[] {
  if (!added.length) return current;

  const incoming = [...added].sort(
    (a, b) => (Date.parse(b.updatedAt) || 0) - (Date.parse(a.updatedAt) || 0),
  );
  const merged: CloudFile[] = new Array(current.length + incoming.length);
  let left = 0;
  let right = 0;
  let out = 0;

  while (left < current.length && right < incoming.length) {
    const currentTime = Date.parse(current[left].updatedAt) || 0;
    const incomingTime = Date.parse(incoming[right].updatedAt) || 0;
    merged[out++] = currentTime >= incomingTime ? current[left++] : incoming[right++];
  }
  while (left < current.length) merged[out++] = current[left++];
  while (right < incoming.length) merged[out++] = incoming[right++];

  return merged;
}

export function useCatalogSync({
  dashboard,
  setDashboard,
  refreshDashboard,
  setNotice,
}: UseCatalogSyncOptions) {
  const [syncBusy, setSyncBusy] = useState(false);
  const [syncDismissed, setSyncDismissed] = useState(false);
  const manualSyncPollingRef = useRef(false);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      manualSyncPollingRef.current = false;
    };
  }, []);

  useEffect(() => {
    if (dashboard?.syncProgress?.active) {
      setSyncDismissed(false);
    }
  }, [dashboard?.syncProgress?.active]);

  useEffect(() => {
    if (
      !dashboard?.syncProgress?.active
      && dashboard?.syncProgress?.phase === "complete"
      && !syncDismissed
    ) {
      const timer = window.setTimeout(() => setSyncDismissed(true), 6000);
      return () => window.clearTimeout(timer);
    }
  }, [
    dashboard?.syncProgress?.active,
    dashboard?.syncProgress?.phase,
    syncDismissed,
  ]);

  const isSyncing = syncBusy || Boolean(dashboard?.syncProgress?.active);

  const handleSync = async () => {
    if (isSyncing || manualSyncPollingRef.current) {
      setNotice("La sincronización ya está en curso y actualizándose en vivo.");
      return;
    }

    setSyncBusy(true);
    setSyncDismissed(false);
    manualSyncPollingRef.current = true;
    let deltaTimer = 0;
    let deltaStopped = false;

    const applyDelta = (delta: Awaited<ReturnType<typeof loadSyncDelta>>) => {
      if (!mountedRef.current) return;
      setDashboard((current) => {
        if (!current) return current;
        if (!delta.files.length) {
          return {
            ...current,
            syncProgress: delta.syncProgress,
            folders: delta.folders ?? current.folders,
          };
        }

        const activeAdded = delta.files.filter((file) => !file.trashed);
        const addedBytes = activeAdded.reduce((sum, file) => sum + file.sizeBytes, 0);
        const addedFavorites = activeAdded.reduce(
          (sum, file) => sum + (file.favorite ? 1 : 0),
          0,
        );
        const nextFiles = mergeRecentFiles(current.files, delta.files);
        const nextFileCount = current.fileCount + activeAdded.length;

        return {
          ...current,
          syncProgress: delta.syncProgress,
          files: nextFiles,
          folders: delta.folders ?? current.folders,
          totalBytes: current.totalBytes + addedBytes,
          fileCount: nextFileCount,
          favoriteCount: current.favoriteCount + addedFavorites,
          recentCount: nextFileCount,
        };
      });
    };

    try {
      // Capture the SQLite cursor first, then publish only newly inserted rows while
      // Telegram keeps scanning. This avoids serializing a large dashboard repeatedly.
      const bootstrap = await loadSyncDelta(null);
      let cursor = bootstrap.cursor;
      applyDelta(bootstrap);

      const pullDelta = async () => {
        if (deltaStopped || !mountedRef.current) return;
        try {
          const delta = await loadSyncDelta(cursor);
          cursor = delta.cursor;
          applyDelta(delta);
        } catch {
          // The final full refresh remains the source of truth after transient reads.
        } finally {
          if (!deltaStopped && mountedRef.current) {
            deltaTimer = window.setTimeout(() => { void pullDelta(); }, 250);
          }
        }
      };

      const syncPromise = syncFiles();
      void pullDelta();
      const count = await syncPromise;
      deltaStopped = true;
      if (deltaTimer) window.clearTimeout(deltaTimer);
      if (mountedRef.current) {
        setNotice(`${count} archivos de Nuvio encontrados en Mensajes guardados.`);
        await refreshDashboard();
      }
    } catch (error) {
      if (mountedRef.current) {
        setNotice(readableError(error));
        await refreshDashboard();
      }
    } finally {
      deltaStopped = true;
      if (deltaTimer) window.clearTimeout(deltaTimer);
      manualSyncPollingRef.current = false;
      if (mountedRef.current) setSyncBusy(false);
    }
  };

  return {
    syncBusy,
    syncDismissed,
    setSyncDismissed,
    isSyncing,
    manualSyncPollingRef,
    handleSync,
  };
}
