import { useState } from "react";
import { FolderOpen, X } from "lucide-react";
import { Dialog } from "../Dialog";
import { readableError } from "../bridge";
import type { CloudFolder } from "../types";

export function folderPathLabel(folder: CloudFolder, folders: CloudFolder[]): string {
  const names = [folder.name];
  const seen = new Set([folder.id]);
  let parent = folder.parentId ?? null;
  while (parent && !seen.has(parent)) {
    seen.add(parent);
    const found = folders.find((item) => item.id === parent);
    if (!found) break;
    names.unshift(found.name);
    parent = found.parentId ?? null;
  }
  return names.join(" / ");
}

export interface MoveToFolderDialogProps {
  folders: CloudFolder[];
  movingFolderId: string | null;
  itemLabel: string;
  onClose: () => void;
  onMove: (folderId: string | null) => Promise<void>;
}

export function MoveToFolderDialog({
  folders,
  movingFolderId,
  itemLabel,
  onClose,
  onMove,
}: MoveToFolderDialogProps) {
  const [target, setTarget] = useState("__root__");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const choices = folders
    .filter((folder) => !folder.trashed && folder.id !== movingFolderId)
    .sort((a, b) =>
      folderPathLabel(a, folders).localeCompare(folderPathLabel(b, folders), "es", {
        numeric: true,
        sensitivity: "base",
      })
    );

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await onMove(target === "__root__" ? null : target);
    } catch (value) {
      setError(readableError(value));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog className="folder-modal" label="Mover a carpeta" onClose={onClose}>
      <button className="icon-button modal-close" onClick={onClose} aria-label="Cerrar">
        <X size={18} />
      </button>
      <div className="modal-brand">
        <FolderOpen size={23} />
      </div>
      <div className="modal-eyebrow">Organización Nuvio</div>
      <h2>Mover a…</h2>
      <p>Elige el destino para {itemLabel}. El cambio se sincronizará mediante Telegram.</p>
      {error && (
        <div className="modal-error" role="alert">
          {error}
        </div>
      )}
      <form className="credential-placeholder" onSubmit={submit}>
        <label htmlFor="folder-target">Destino</label>
        <select id="folder-target" value={target} onChange={(event) => setTarget(event.target.value)}>
          <option value="__root__">Mi unidad</option>
          {choices.map((folder) => (
            <option key={folder.id} value={folder.id}>
              {folderPathLabel(folder, folders)}
            </option>
          ))}
        </select>
        <button className="primary-button modal-primary" disabled={busy} type="submit">
          {busy ? "Moviendo…" : "Mover"}
        </button>
      </form>
    </Dialog>
  );
}
