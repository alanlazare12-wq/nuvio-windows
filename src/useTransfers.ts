import { useMemo, useRef, useState, type Dispatch, type SetStateAction } from "react";
import {
  clearTransferHistory,
  deleteVerifiedUploadSource,
  queueDownloads,
  selectDownloadDirectory,
} from "./bridge/transfers";
import { readableError } from "./bridge/shared";
import { isTransferPending } from "./components/TransferRows";
import type {
  DashboardData,
  TransferFilter,
  TransferJob,
} from "./types";

type UseTransfersOptions = {
  dashboard: DashboardData | null;
  selectedFiles: Set<string>;
  clearSelection: () => void;
  onRequireConnection: () => void;
  refreshDashboard: () => Promise<DashboardData | undefined>;
  setNotice: Dispatch<SetStateAction<string | null>>;
};

export function useTransfers({
  dashboard,
  selectedFiles,
  clearSelection,
  onRequireConnection,
  refreshDashboard,
  setNotice,
}: UseTransfersOptions) {
  const [transferFilter, setTransferFilter] = useState<TransferFilter>("all");
  const downloadBusyRef = useRef(false);

  const filteredTransfers = useMemo(() => {
    if (!dashboard) return [];
    return dashboard.transfers.filter((job) => {
      if (transferFilter === "failed") return job.status === "failed";
      if (transferFilter === "completed") {
        return job.status === "completed" || job.status === "duplicate";
      }
      if (transferFilter === "pending") return isTransferPending(job);
      return true;
    });
  }, [dashboard, transferFilter]);

  const handleBulkDownload = async (ids = [...selectedFiles]) => {
    if (!ids.length || downloadBusyRef.current) return;
    if (!dashboard?.telegramConnected) {
      onRequireConnection();
      return;
    }
    if (ids.length > 500) {
      setNotice("Selecciona como máximo 500 archivos por lote.");
      return;
    }

    downloadBusyRef.current = true;
    try {
      const directory = await selectDownloadDirectory();
      if (!directory) return;

      const results = await queueDownloads(
        ids,
        directory,
        dashboard.settings.conflictPolicy,
      );
      const queued = results.filter((item) => item.status === "queued").length;
      const skipped = results.filter((item) => item.status === "skipped").length;
      const errors = results.filter((item) => item.status === "error").length;

      setNotice(
        [
          queued ? `${queued} descarga${queued === 1 ? "" : "s"} en cola` : null,
          skipped ? `${skipped} omitido${skipped === 1 ? "" : "s"}` : null,
          errors ? `${errors} error${errors === 1 ? "" : "es"}` : null,
        ].filter(Boolean).join(" · ") || "Sin cambios",
      );
      clearSelection();
      await refreshDashboard();
    } catch (error) {
      setNotice(readableError(error));
    } finally {
      downloadBusyRef.current = false;
    }
  };

  const handleClearHistory = async () => {
    if (!dashboard?.transferHistory.length) return;
    if (
      !window.confirm(
        "¿Limpiar el historial de transferencias? Los archivos almacenados no se eliminarán.",
      )
    ) {
      return;
    }

    try {
      const removed = await clearTransferHistory();
      setNotice(
        `${removed} transferencia${removed === 1 ? "" : "s"} eliminada${removed === 1 ? "" : "s"} del historial.`,
      );
      await refreshDashboard();
    } catch (error) {
      setNotice(readableError(error));
    }
  };

  const handleDeleteVerifiedSource = async (job: TransferJob) => {
    if (!job.sourceDeleteAvailable) return;
    if (
      !window.confirm(
        `¿Borrar el original local de “${job.fileName}”?\n\nNuvio volverá a comprobar tamaño y SHA-256 antes de eliminarlo. La copia confirmada en Telegram no se borra.`,
      )
    ) {
      return;
    }

    try {
      const removed = await deleteVerifiedUploadSource(job.id);
      setNotice(
        removed
          ? `Original de “${job.fileName}” eliminado después de verificarlo.`
          : "No había un original pendiente de eliminar.",
      );
      await refreshDashboard();
    } catch (error) {
      setNotice(readableError(error));
      await refreshDashboard();
    }
  };

  return {
    transferFilter,
    setTransferFilter,
    filteredTransfers,
    handleBulkDownload,
    handleClearHistory,
    handleDeleteVerifiedSource,
  };
}
