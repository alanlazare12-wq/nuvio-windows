import { useState, type Dispatch, type SetStateAction } from "react";
import {
  deleteUploadedImageSources,
  getUploadedImageCleanupSummary,
} from "./bridge/media";
import { exportDiagnostics } from "./bridge/settings";
import { readableError } from "./bridge/shared";
import { formatBytes } from "./format";
import type { DashboardData } from "./types";

type UseMaintenanceActionsOptions = {
  refreshDashboard: () => Promise<DashboardData | undefined>;
  setNotice: Dispatch<SetStateAction<string | null>>;
};

export function useMaintenanceActions({
  refreshDashboard,
  setNotice,
}: UseMaintenanceActionsOptions) {
  const [cleanupBusy, setCleanupBusy] = useState(false);

  const handleExportDiagnostics = async () => {
    try {
      if (await exportDiagnostics()) {
        setNotice("Diagnóstico de Nuvio exportado sin credenciales ni códigos de acceso.");
      }
    } catch (error) {
      setNotice(readableError(error));
    }
  };

  const handleFreeUploadedImages = async () => {
    if (cleanupBusy) return;

    setCleanupBusy(true);
    try {
      const summary = await getUploadedImageCleanupSummary();
      if (!summary.count) {
        setNotice(
          "No hay originales locales rastreados pendientes. Nuvio solo puede borrar con seguridad archivos cuya ruta y SHA-256 quedaron registrados durante la subida; las subidas antiguas cuyo historial se eliminó antes de ese registro no se pueden localizar automáticamente.",
        );
        return;
      }

      if (!window.confirm(
        `¿Liberar espacio borrando ${summary.count} ${summary.count === 1 ? "imagen" : "imágenes"} ya ${summary.count === 1 ? "subida" : "subidas"}?\n\n`
        + `Espacio potencial: ${formatBytes(summary.bytes)}. Nuvio comprobará tamaño y SHA-256 de cada original antes de borrarlo. `
        + "La copia guardada en Telegram no se elimina. No importa si ‘Liberar espacio tras subir’ estaba desactivado, siempre que Nuvio haya registrado el original durante esa subida.",
      )) {
        return;
      }

      const result = await deleteUploadedImageSources();
      const parts = [
        `${result.deleted} ${result.deleted === 1 ? "imagen liberada" : "imágenes liberadas"}`,
        result.releasedBytes > 0 ? `${formatBytes(result.releasedBytes)} liberados` : null,
        result.skipped ? `${result.skipped} omitida${result.skipped === 1 ? "" : "s"}` : null,
        result.failed ? `${result.failed} no se pudieron borrar y se conservaron` : null,
      ].filter(Boolean);

      setNotice(parts.join(" · "));
      await refreshDashboard();
    } catch (error) {
      setNotice(readableError(error));
      await refreshDashboard();
    } finally {
      setCleanupBusy(false);
    }
  };

  return {
    cleanupBusy,
    handleExportDiagnostics,
    handleFreeUploadedImages,
  };
}
