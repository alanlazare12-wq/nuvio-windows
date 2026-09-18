import { useEffect, useRef, type MutableRefObject } from "react";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { updateUploadNotification } from "./bridge/mobile";
import { isTransferPending } from "./components/TransferRows";
import type { DashboardData } from "./types";

type UseDashboardLifecycleOptions = {
  dashboard: DashboardData | null;
  refreshDashboard: () => Promise<DashboardData | undefined>;
  dragActiveRef: MutableRefObject<boolean>;
  manualSyncPollingRef: MutableRefObject<boolean>;
  invalidateDashboardRequests: () => void;
  setNotice: (message: string | null) => void;
};

export function useDashboardLifecycle({
  dashboard,
  refreshDashboard,
  dragActiveRef,
  manualSyncPollingRef,
  invalidateDashboardRequests,
  setNotice,
}: UseDashboardLifecycleOptions) {
  const previousUploadActiveRef = useRef(false);
  const previousUploadKeyRef = useRef("");
  const previousPendingRef = useRef<number | null>(null);

  useEffect(() => {
    let disposed = false;
    let timer = 0;

    const poll = async () => {
      if (disposed) return;
      if (dragActiveRef.current) {
        timer = window.setTimeout(poll, 800);
        return;
      }
      if (manualSyncPollingRef.current) {
        timer = window.setTimeout(poll, 900);
        return;
      }

      const next = await refreshDashboard();
      const active = next?.syncProgress?.active || (next?.queueSummary.pending ?? 0) > 0;
      if (!disposed) {
        timer = window.setTimeout(
          poll,
          active ? 1200 : document.hidden ? 8000 : 3000,
        );
      }
    };

    void poll();
    return () => {
      disposed = true;
      invalidateDashboardRequests();
      window.clearTimeout(timer);
    };
  }, []);

  useEffect(() => {
    if (!dashboard) return;

    const q = dashboard.queueSummary;
    const activeUploads = dashboard.transfers.filter(
      (transfer) => transfer.direction === "upload" && isTransferPending(transfer),
    );
    const hasActive = activeUploads.length > 0 || (q.pending > 0 && q.active > 0);
    const wasActive = previousUploadActiveRef.current;

    if (hasActive) {
      previousUploadActiveRef.current = true;
      const currentFileName = activeUploads[0]?.fileName ?? null;
      const pct = q.totalBytes > 0
        ? Math.round((q.processedBytes / q.totalBytes) * 100)
        : null;
      const key = [
        pct,
        q.completed,
        q.pending,
        currentFileName,
        Math.round(q.speedBps / 50000),
      ].join(":");

      if (key !== previousUploadKeyRef.current) {
        previousUploadKeyRef.current = key;
        void updateUploadNotification({
          active: true,
          total: q.total,
          completed: q.completed,
          pending: q.pending,
          failed: q.failed,
          percent: pct,
          processedBytes: q.processedBytes,
          totalBytes: q.totalBytes,
          speedBps: q.speedBps,
          currentFileName,
        });
      }
    } else if (wasActive) {
      previousUploadActiveRef.current = false;
      previousUploadKeyRef.current = "";
      void updateUploadNotification({
        active: false,
        total: q.total,
        completed: q.completed,
        pending: 0,
        failed: q.failed,
        percent: 100,
        processedBytes: q.totalBytes,
        totalBytes: q.totalBytes,
        speedBps: 0,
      });
    }
  }, [
    dashboard?.queueSummary?.pending,
    dashboard?.queueSummary?.completed,
    dashboard?.queueSummary?.processedBytes,
    dashboard?.queueSummary?.speedBps,
    dashboard?.transfers,
  ]);

  useEffect(() => {
    if (!dashboard) return;

    const pending = dashboard.queueSummary.pending;
    const previous = previousPendingRef.current;
    previousPendingRef.current = pending;
    if (previous == null || previous <= 0 || pending !== 0) return;

    const failed = dashboard.queueSummary.failed;
    setNotice(
      failed > 0
        ? `La cola terminó con ${failed} transferencia${failed === 1 ? "" : "es"} con error.`
        : `Cola completada · ${dashboard.fileCount} archivo${dashboard.fileCount === 1 ? "" : "s"} sincronizado${dashboard.fileCount === 1 ? "" : "s"}.`,
    );

    void (async () => {
      try {
        let granted = await isPermissionGranted();
        if (!granted) granted = (await requestPermission()) === "granted";
        if (!granted) return;

        sendNotification({
          title: "Nuvio · Cola finalizada",
          body: failed > 0
            ? `La cola terminó con ${failed} transferencia${failed === 1 ? "" : "es"} fallida${failed === 1 ? "" : "s"}.`
            : `${dashboard.fileCount} archivo${dashboard.fileCount === 1 ? "" : "s"} sincronizado${dashboard.fileCount === 1 ? "" : "s"}.`,
        });
      } catch {
        // Notifications are optional and must never block transfers.
      }
    })();
  }, [
    dashboard?.queueSummary.pending,
    dashboard?.queueSummary.failed,
    dashboard?.queueSummary.total,
    dashboard?.fileCount,
    dashboard?.telegramConnected,
  ]);
}
