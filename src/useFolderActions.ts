import { useState, type Dispatch, type SetStateAction } from "react";
import {
  createFolder,
  deleteFolder,
  moveFilesToFolder,
  moveFolder,
  renameFolder,
} from "./bridge/files";
import { readableError } from "./bridge/shared";
import type { CloudFolder, DashboardData } from "./types";

type FolderEditorState = {
  mode: "create" | "rename";
  folder?: CloudFolder;
};

type MoveDialogState =
  | { kind: "files"; ids: string[] }
  | { kind: "folder"; folder: CloudFolder };

type UseFolderActionsOptions = {
  currentFolderId: string | null;
  refreshDashboard: () => Promise<DashboardData | undefined>;
  setSelectedFiles: Dispatch<SetStateAction<Set<string>>>;
  setNotice: Dispatch<SetStateAction<string | null>>;
};

export function useFolderActions({
  currentFolderId,
  refreshDashboard,
  setSelectedFiles,
  setNotice,
}: UseFolderActionsOptions) {
  const [folderEditor, setFolderEditor] = useState<FolderEditorState | null>(null);
  const [moveDialog, setMoveDialog] = useState<MoveDialogState | null>(null);

  const handleSaveFolder = async (name: string) => {
    if (!folderEditor) return;

    try {
      if (folderEditor.mode === "create") {
        await createFolder(name, currentFolderId);
        setNotice(`Carpeta “${name.trim()}” creada y sincronizada con Telegram.`);
      } else if (folderEditor.folder) {
        await renameFolder(folderEditor.folder.id, name);
        setNotice(`Carpeta renombrada a “${name.trim()}”.`);
      }
      setFolderEditor(null);
      await refreshDashboard();
    } catch (error) {
      throw new Error(readableError(error));
    }
  };

  const handleMoveConfirm = async (targetFolderId: string | null) => {
    if (!moveDialog) return;

    try {
      if (moveDialog.kind === "files") {
        const changed = await moveFilesToFolder(moveDialog.ids, targetFolderId);
        setSelectedFiles(new Set());
        setNotice(
          `${changed} archivo${changed === 1 ? "" : "s"} movido${changed === 1 ? "" : "s"}.`,
        );
      } else {
        await moveFolder(moveDialog.folder.id, targetFolderId);
        setNotice(`Carpeta “${moveDialog.folder.name}” movida.`);
      }
      setMoveDialog(null);
      await refreshDashboard();
    } catch (error) {
      throw new Error(readableError(error));
    }
  };

  const handleDeleteFolder = async (folder: CloudFolder) => {
    if (!window.confirm(
      `¿Eliminar la carpeta “${folder.name}”?\n\nPor seguridad, Nuvio solo elimina carpetas vacías.`,
    )) {
      return;
    }

    try {
      await deleteFolder(folder.id);
      setNotice(`Carpeta “${folder.name}” eliminada.`);
      await refreshDashboard();
    } catch (error) {
      setNotice(readableError(error));
    }
  };

  return {
    folderEditor,
    setFolderEditor,
    moveDialog,
    setMoveDialog,
    handleSaveFolder,
    handleMoveConfirm,
    handleDeleteFolder,
  };
}
