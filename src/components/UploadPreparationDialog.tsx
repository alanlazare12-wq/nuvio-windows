import { Archive, Files, HardDrive, Upload, X } from "lucide-react";
import { Dialog } from "../Dialog";
import type { UploadAdvisory, UploadPreparationDecision } from "../types";

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1000)), units.length - 1);
  const value = bytes / (1000 ** index);
  return `${value >= 100 || index === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[index]}`;
}

export function UploadPreparationDialog({
  advisory,
  onChoose,
}: {
  advisory: UploadAdvisory;
  onChoose: (decision: UploadPreparationDecision) => void;
}) {
  const split = advisory.recommendation === "splitVolumes";
  const manyImages = advisory.reasonCodes.includes("manyImages");
  const largeSingleFile = advisory.fileCount === 1 && advisory.reasonCodes.includes("largeFile");
  const primaryLabel = split ? "Dividir en volúmenes y subir" : "Comprimir y agrupar";
  const title = split
    ? advisory.oversizedCount === 1
      ? "Este archivo supera 2 GB"
      : `${advisory.oversizedCount} archivos superan 2 GB`
    : manyImages
      ? "Seleccionaste muchas imágenes"
      : largeSingleFile
        ? "Este archivo es grande"
        : "Esta selección es grande";

  const description = split
    ? "Nuvio puede dividir los archivos grandes en ZIP independientes de hasta 2 GB, verificar cada parte con SHA-256 y conservar los originales."
    : manyImages
      ? "Puedes agrupar estas imágenes en uno o varios ZIP para que el respaldo quede más ordenado. La decisión es tuya."
      : largeSingleFile
        ? "Este archivo supera 1 GB. Puedes comprimirlo antes de subirlo o continuar sin cambios."
        : "Nuvio puede agrupar la selección en uno o varios ZIP de hasta 2 GB para simplificar el respaldo.";

  return (
    <Dialog
      className="upload-preparation-modal"
      labelledBy="upload-preparation-title"
      onClose={() => onChoose("cancel")}
    >
      <button className="icon-button modal-close" onClick={() => onChoose("cancel")} aria-label="Cerrar">
        <X size={18} />
      </button>
      <div className="upload-preparation-icon">
        <Archive size={24} />
      </div>
      <div className="modal-eyebrow">Preparación inteligente · Tú decides</div>
      <h2 id="upload-preparation-title">{title}</h2>
      <p>{description}</p>

      <div className="upload-preparation-stats" aria-label="Resumen de la selección">
        <div>
          <Files size={16} />
          <span>Archivos</span>
          <strong>{advisory.fileCount}</strong>
        </div>
        <div>
          <HardDrive size={16} />
          <span>Tamaño conocido</span>
          <strong>{formatBytes(advisory.totalBytes)}</strong>
        </div>
        <div>
          <Upload size={16} />
          <span>Límite Telegram</span>
          <strong>{formatBytes(advisory.telegramLimitBytes)}</strong>
        </div>
      </div>

      {advisory.imageCount > 0 && (
        <div className="upload-preparation-note">
          {advisory.imageCount} imagen{advisory.imageCount === 1 ? "" : "es"} detectada{advisory.imageCount === 1 ? "" : "s"}.
        </div>
      )}

      {split && advisory.oversizedFiles.length > 0 && (
        <div className="upload-preparation-files">
          {advisory.oversizedFiles.map((file) => (
            <div key={file.name}>
              <span title={file.name}>{file.name}</span>
              <strong>{formatBytes(file.sizeBytes)}</strong>
            </div>
          ))}
          {advisory.oversizedCount > advisory.oversizedFiles.length && (
            <small>Y {advisory.oversizedCount - advisory.oversizedFiles.length} archivo(s) grande(s) más.</small>
          )}
        </div>
      )}

      {advisory.telegramOversizedCount > 0 && (
        <div className="upload-preparation-warning" role="note">
          Si eliges <strong>subir sin cambios</strong>, {advisory.telegramOversizedCount} archivo{advisory.telegramOversizedCount === 1 ? "" : "s"} supera{advisory.telegramOversizedCount === 1 ? "" : "n"} el límite actual de Telegram y Nuvio lo{advisory.telegramOversizedCount === 1 ? "" : "s"} omitirá.
        </div>
      )}

      {advisory.unknownSizeCount > 0 && (
        <div className="upload-preparation-note">
          {advisory.unknownSizeCount} archivo(s) no pudieron medirse antes de obtener acceso completo; se validarán al preparar la subida.
        </div>
      )}

      <div className="upload-preparation-actions">
        <button className="primary-button" onClick={() => onChoose("archive")}>{primaryLabel}</button>
        <button className="secondary-button" onClick={() => onChoose("direct")}>
          No, subir sin cambios
        </button>
        <button className="text-button" onClick={() => onChoose("cancel")}>Cancelar</button>
      </div>
      <small className="upload-preparation-footnote">
        Nuvio nunca comprime ni divide esta selección sin tu autorización, salvo que actives previamente la casilla automática.
      </small>
    </Dialog>
  );
}
