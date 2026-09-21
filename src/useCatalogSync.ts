import {
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";
import { cancelSync, loadSyncDelta, syncFiles } from "./bridge/dashboard";
import { readableError } from "./bridge/shared";
import type { DashboardData } from "./types";

type UseCatalogSyncOptions = {
  dashboard: DashboardData | null;
  setDashboard: Dispatch<SetStateAction<DashboardData | null>>;
  refreshDashboard: () => Promise<DashboardData | undefined>;
  setNotice: Dispatch<SetStateAction<string | null>>;
};

export function useCatalogSync({
  dashboard,
  setDashboard,
  refreshDashboard,
  setNotice,
}: UseCatalogSyncOptions) {
  const [syncBusy, setSyncBusy] = useState(false);
  const [syncCancelBusy, setSyncCancelBusy] = useState(false);
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

  const handleStopSync = async () => {
    if (!isSyncing || syncCancelBusy) return;
    setSyncCancelBusy(true);
    try {
      const requested = await cancelSync();
      setNotice(requested
        ? "Deteniendo sincronización de forma segura…"
        : "La sincronización ya había terminado.");
    } catch (error) {
      setNotice(readableError(error));
    } finally {
      if (mountedRef.current) setSyncCancelBusy(false);
    }
  };

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
      setDashboard((current) => current ? {
        ...current,
        syncProgress: delta.syncProgress,
        folders: delta.folders ?? current.folders,
        catalogCursor: delta.cursor,
      } : current);
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
        const message = readableError(error);
        setNotice(message.includes("Sincronización detenida por el usuario")
          ? "Sincronización detenida. El catálogo queda guardado hasta el último checkpoint seguro."
          : message);
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
    syncCancelBusy,
    syncDismissed,
    setSyncDismissed,
    isSyncing,
    manualSyncPollingRef,
    handleSync,
    handleStopSync,
  };
}
