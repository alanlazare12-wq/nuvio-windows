import { useState } from "react";
import { FolderPlus, X } from "lucide-react";
import { Dialog } from "../Dialog";
import { readableError } from "../bridge";

export interface FolderEditorDialogProps {
  mode: "create" | "rename";
  initialName: string;
  parentName: string;
  onClose: () => void;
  onSave: (name: string) => Promise<void>;
}

export function FolderEditorDialog({
  mode,
  initialName,
  parentName,
  onClose,
  onSave,
}: FolderEditorDialogProps) {
  const [name, setName] = useState(initialName);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    const normalized = name.trim();
    if (!normalized) return setError("Escribe un nombre para la carpeta");
    setBusy(true);
    setError(null);
    try {
      await onSave(normalized);
    } catch (value) {
      setError(readableError(value));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog className="folder-modal" label={mode === "create" ? "Nueva carpeta" : "Renombrar carpeta"} onClose={onClose}>
      <button className="icon-button modal-close" onClick={onClose} aria-label="Cerrar">
        <X size={18} />
      </button>
      <div className="modal-brand">
        <FolderPlus size={23} />
      </div>
      <div className="modal-eyebrow">{parentName}</div>
      <h2>{mode === "create" ? "Nueva carpeta" : "Renombrar carpeta"}</h2>
      <p>
        {mode === "create"
          ? "La estructura se sincronizará con Telegram y aparecerá igual en Windows y Android."
          : "El nuevo nombre se sincronizará en tus dispositivos."}
      </p>
      {error && (
        <div className="modal-error" role="alert">
          {error}
        </div>
      )}
      <form className="credential-placeholder" onSubmit={submit}>
        <label htmlFor="folder-name">Nombre</label>
        <input
          id="folder-name"
          autoFocus
          maxLength={120}
          value={name}
          onChange={(event) => setName(event.target.value)}
        />
        <button className="primary-button modal-primary" disabled={busy} type="submit">
          {busy ? "Guardando…" : mode === "create" ? "Crear carpeta" : "Guardar nombre"}
        </button>
      </form>
    </Dialog>
  );
}
