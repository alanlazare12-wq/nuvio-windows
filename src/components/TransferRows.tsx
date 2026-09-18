import {
  ArrowDownToLine,
  ArrowUpFromLine,
  Check,
  Pause,
  Play,
  RotateCcw,
  Trash2,
  X,
} from "lucide-react";
import {
  cancelTransfer,
  pauseTransfer,
  resumeTransfer,
  retryTransfer,
} from "../bridge/transfers";
import { formatBytes, formatEta, formatSpeed } from "../format";
import type { TransferJob } from "../types";

function phaseLabel(job: TransferJob): string {
  const labels: Record<string, string> = {
    waiting: "Esperando",
    analyzing: "Analizando",
    copying: "Copiando",
    ready: "Listo para subir",
    queued: "En cola",
    uploading: "Subiendo",
    downloading: "Descargando",
    confirming: "Confirmando",
    retry_wait: "Esperando reintento",
    running: "Procesando",
    paused: "Pausado",
    completed: "Completado",
    failed: "Error",
    error: "Error",
    duplicate: "Duplicado",
    cancelled: "Cancelado",
  };
  return labels[job.phase] ?? labels[job.status] ?? job.status;
}

function transferDate(unixSeconds: number): string {
  if (!Number.isFinite(unixSeconds) || unixSeconds <= 0) return "Sin fecha";
  return new Intl.DateTimeFormat("es-MX", {
    day: "2-digit",
    month: "short",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(unixSeconds * 1000));
}

export function isTransferPending(job: TransferJob): boolean {
  return !["completed", "failed", "duplicate", "cancelled"].includes(job.status);
}

type TransferRowProps = {
  job: TransferJob;
  connected: boolean;
  onAction: (operation: () => Promise<unknown>) => void;
};

export function TransferRow({ job, connected, onAction }: TransferRowProps) {
  const calculating = [
    "analyzing",
    "copying",
    "uploading",
    "downloading",
    "confirming",
    "running",
  ].includes(job.status);

  return (
    <article className={`transfer-detail status-${job.status}`}>
      <div className="transfer-direction">
        {job.direction === "upload"
          ? <ArrowUpFromLine size={16} />
          : <ArrowDownToLine size={16} />}
      </div>
      <div className="transfer-main">
        <div className="transfer-title-line">
          <strong>{job.fileName}</strong>
          <span className="phase-badge">{phaseLabel(job)}</span>
        </div>
        <div className="transfer-stats">
          <span>{formatBytes(job.processedBytes)} / {formatBytes(job.totalBytes)}</span>
          <span>{formatSpeed(job.speedBps)}</span>
          <span>{formatEta(job.etaSeconds, calculating && job.speedBps <= 0)}</span>
          {job.attempts > 0 && (
            <span>
              Intento {Math.min(job.attempts + 1, job.maxAttempts)}/{job.maxAttempts}
            </span>
          )}
        </div>
        <progress max={100} value={job.progress} aria-label={`Progreso ${job.fileName}`} />
        {job.error && <p className="transfer-error">{job.error}</p>}
      </div>
      <div className="transfer-controls">
        {job.canPause && (
          <button
            className="ghost-icon"
            onClick={() => onAction(() => pauseTransfer(job.id))}
            title="Pausar"
          >
            <Pause size={15} />
          </button>
        )}
        {(job.status === "paused" || job.status === "cancelled") && (
          <button
            className="ghost-icon"
            disabled={!connected && job.direction === "upload"}
            onClick={() => onAction(() => resumeTransfer(job.id))}
            title="Continuar"
          >
            <Play size={15} />
          </button>
        )}
        {job.status === "failed" && (
          <button
            className="ghost-icon"
            disabled={!connected}
            onClick={() => onAction(() => retryTransfer(job.id))}
            title="Reintentar"
          >
            <RotateCcw size={15} />
          </button>
        )}
        {job.canCancel && (
          <button
            className="ghost-icon danger"
            onClick={() => onAction(() => cancelTransfer(job.id))}
            title="Cancelar"
          >
            <X size={15} />
          </button>
        )}
        {job.status === "completed" && <Check size={18} className="success-icon" />}
      </div>
    </article>
  );
}

type TransferHistoryRowProps = {
  job: TransferJob;
  onDeleteSource: () => void;
};

export function TransferHistoryRow({
  job,
  onDeleteSource,
}: TransferHistoryRowProps) {
  const terminalLabel =
    job.status === "completed"
      ? "Completado"
      : job.status === "duplicate"
        ? "Duplicado"
        : job.status === "cancelled"
          ? "Cancelado"
          : phaseLabel(job);

  return (
    <article className={`history-row status-${job.status}`}>
      <div className="transfer-direction">
        {job.direction === "upload"
          ? <ArrowUpFromLine size={16} />
          : <ArrowDownToLine size={16} />}
      </div>
      <div className="history-row-copy">
        <strong>{job.fileName}</strong>
        <span>
          {job.direction === "upload" ? "Subida" : "Descarga"} · {formatBytes(job.totalBytes)}
        </span>
        {job.sourceDeleted && (
          <small className="source-cleanup-ok">Original local eliminado</small>
        )}
        {job.sourceDeleteError && !job.sourceDeleted && (
          <small className="source-cleanup-error">{job.sourceDeleteError}</small>
        )}
      </div>
      <span className="phase-badge">{terminalLabel}</span>
      <div className="history-row-actions">
        {job.sourceDeleteAvailable && (
          <button className="secondary-button compact" onClick={onDeleteSource}>
            <Trash2 size={13} /> Borrar original
          </button>
        )}
        <time>{transferDate(job.updatedAt)}</time>
      </div>
    </article>
  );
}
