import { AlertTriangle, Check, Copy, File, X } from "lucide-react";
import { useState } from "react";
import { Dialog } from "../Dialog";
import { formatBytes } from "../format";
import type { SkippedUploadItem } from "../types";

type SkippedUploadsModalProps = {
  items: SkippedUploadItem[];
  isPremium: boolean;
  onClose: () => void;
};

export function SkippedUploadsModal({
  items,
  isPremium,
  onClose,
}: SkippedUploadsModalProps) {
  const [copied, setCopied] = useState(false);

  const copyList = async () => {
    try {
      const text = items
        .map(
          (item, index) =>
            `${index + 1}. ${item.fileName}\n   Ruta: ${item.path}\n   Motivo: ${item.reason}${
              item.sizeBytes ? ` (${formatBytes(item.sizeBytes)})` : ""
            }`,
        )
        .join("\n\n");
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2500);
    } catch {
      // Clipboard can be unavailable in restricted webviews.
    }
  };

  const limitGb = isPremium ? 4 : 2;

  return (
    <Dialog className="skipped-modal" labelledBy="skipped-title" onClose={onClose}>
      <button className="icon-button modal-close" onClick={onClose} aria-label="Cerrar">
        <X size={18} />
      </button>
      <div className="skipped-icon-badge">
        <AlertTriangle size={24} />
      </div>
      <div className="modal-eyebrow">Límite de Telegram · Archivos omitidos</div>
      <div className="skipped-modal-header">
        <h2 id="skipped-title">Archivos no subidos ({items.length})</h2>
        <p>
          Telegram limita la subida a un máximo de <strong>{limitGb} GB por archivo</strong>
          {isPremium
            ? " (con tu suscripción Telegram Premium)"
            : " (o 4 GB con Telegram Premium)"}.
          Nuvio omitió los siguientes archivos para que el resto de tu contenido
          continúe subiéndose con normalidad.
        </p>
      </div>

      <div
        className="skipped-list-container"
        role="region"
        aria-label="Lista de archivos omitidos"
      >
        {items.map((item, index) => (
          <div key={`${item.path}-${index}`} className="skipped-item-card">
            <div className="skipped-item-main">
              <div className="skipped-file-icon">
                <File size={18} />
              </div>
              <div className="skipped-file-details">
                <span className="skipped-file-name" title={item.fileName}>
                  {item.fileName}
                </span>
                <span className="skipped-file-path" title={item.path}>
                  {item.path}
                </span>
              </div>
            </div>
            <div className="skipped-item-status">
              <span className="skipped-reason-badge" title={item.reason}>
                <AlertTriangle size={12} />
                {item.reason}
              </span>
              {item.sizeBytes != null && item.sizeBytes > 0 && (
                <span className="file-meta">{formatBytes(item.sizeBytes)}</span>
              )}
            </div>
          </div>
        ))}
      </div>

      <div className="skipped-modal-actions">
        <button
          type="button"
          className="secondary-button"
          onClick={() => void copyList()}
          title="Copiar lista de archivos al portapapeles"
        >
          {copied ? <Check size={16} /> : <Copy size={16} />}
          <span>{copied ? "¡Copiado!" : "Copiar lista"}</span>
        </button>
        <button
          type="button"
          className="primary-button modal-primary"
          style={{ width: "auto", margin: 0 }}
          onClick={onClose}
        >
          Entendido
        </button>
      </div>
    </Dialog>
  );
}
