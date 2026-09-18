import { type Dispatch, type SetStateAction } from "react";
import {
  deleteFilesPermanently,
  emptyTrash,
  setTrashedMany,
} from "./bridge/files";
import { readableError } from "./bridge/shared";
import type { DashboardData } from "./types";

type UseTrashActionsOptions = {
  dashboard: DashboardData | null;
  mediaFileId: string | null;
  closeMedia: () => void;
  setSelectedFiles: Dispatch<SetStateAction<Set<string>>>;
  refreshDashboard: () => Promise<DashboardData | undefined>;
  setNotice: Dispatch<SetStateAction<string | null>>;
  onRequireConnection: () => void;
};

export function useTrashActions({
  dashboard,
  mediaFileId,
  closeMedia,
  setSelectedFiles,
  refreshDashboard,
  setNotice,
  onRequireConnection,
}: UseTrashActionsOptions) {
  const handleBulkTrash = async (ids: string[], trashed: boolean) => {
    if (!ids.length) return;

    try {
      const changed = await setTrashedMany(ids, trashed);
      setSelectedFiles(new Set());
      setNotice(
        trashed
          ? `${changed} archivo${changed === 1 ? "" : "s"} movido${changed === 1 ? "" : "s"} a la Papelera.`
          : `${changed} archivo${changed === 1 ? "" : "s"} restaurado${changed === 1 ? "" : "s"}.`,
      );
      await refreshDashboard();
    } catch (error) {
      setNotice(readableError(error));
    }
  };

  const handlePermanentDelete = async (ids: string[]) => {
    if (!ids.length) return;
    if (!dashboard?.telegramConnected) {
      onRequireConnection();
      return;
    }

    const count = ids.length;
    const confirmed = window.confirm(
      `¿Eliminar definitivamente ${count} archivo${count === 1 ? "" : "s"}?\n\nTambién se eliminará${count === 1 ? "" : "n"} de Mensajes guardados de Telegram. Esta acción no se puede deshacer.`,
    );
    if (!confirmed) return;

    try {
      const removed = await deleteFilesPermanently(ids);
      if (mediaFileId && ids.includes(mediaFileId)) closeMedia();
      setSelectedFiles(new Set());
      setNotice(
        `${removed} archivo${removed === 1 ? "" : "s"} eliminado${removed === 1 ? "" : "s"} definitivamente.`,
      );
      await refreshDashboard();
    } catch (error) {
      setNotice(readableError(error));
    }
  };

  const handleEmptyTrash = async () => {
    const trashCount = dashboard?.trashCount ?? 0;
    if (!trashCount) return;
    if (!dashboard?.telegramConnected) {
      onRequireConnection();
      return;
    }

    const confirmed = window.confirm(
      `¿Vaciar la Papelera?\n\nSe eliminarán definitivamente ${trashCount} archivo${trashCount === 1 ? "" : "s"} de Nuvio y de Mensajes guardados de Telegram. Esta acción no se puede deshacer.`,
    );
    if (!confirmed) return;

    try {
      const removed = await emptyTrash();
      closeMedia();
      setSelectedFiles(new Set());
      setNotice(
        `Papelera vaciada · ${removed} archivo${removed === 1 ? "" : "s"} eliminado${removed === 1 ? "" : "s"}.`,
      );
      await refreshDashboard();
    } catch (error) {
      setNotice(readableError(error));
    }
  };

  return {
    handleBulkTrash,
    handlePermanentDelete,
    handleEmptyTrash,
  };
}
