import { X } from "lucide-react";
import type { SyncProgress } from "../types";

export interface SyncProgressPanelProps {
  syncProgress?: SyncProgress | null;
  syncBusy: boolean;
  onDismiss?: () => void;
}

export function SyncProgressPanel({ syncProgress, syncBusy, onDismiss }: SyncProgressPanelProps) {
  if (!syncProgress?.phase && !syncBusy) return null;

  const active = Boolean(syncProgress?.active);
  const phase = syncProgress?.phase;
  const isFolders = phase === "folders";
  const isFiles = phase === "files";
  const isApplying = phase === "applying";
  const isComplete = phase === "complete";
  const hasError = Boolean(syncProgress?.error);

  const title = active
    ? isFolders
      ? "Cargando carpetas…"
      : isFiles
      ? "Sincronizando archivos…"
      : isApplying
      ? "Actualizando catálogo…"
      : "Sincronizando con Telegram…"
    : syncBusy
    ? "Iniciando sincronización…"
    : hasError
    ? "Sincronización interrumpida"
    : "Sincronización completada";

  const percentLabel = active
    ? syncProgress?.percent == null
      ? "Calculando…"
      : `${syncProgress.percent}% aprox.`
    : syncBusy
    ? "Calculando…"
    : hasError
    ? "Pendiente de reintentar"
    : "100%";

  const detailLabel =
    syncProgress?.error ||
    (active
      ? isFolders
        ? "Preparando estructura de carpetas antes de mostrar archivos…"
        : `${syncProgress?.scanned ?? 0} mensajes revisados${
            syncProgress?.etaSeconds != null
              ? ` · Quedan aproximadamente ${
                  syncProgress.etaSeconds < 60
                    ? `${syncProgress.etaSeconds} s`
                    : `${Math.ceil(syncProgress.etaSeconds / 60)} min`
                }`
              : ""
          }`
      : syncBusy
      ? "Consultando Telegram…"
      : "Catálogo actualizado");

  return (
    <section className="sync-progress-panel" aria-label="Progreso de sincronización">
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
        <strong>{title}</strong>
        <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
          <span>{percentLabel}</span>
          {!active && onDismiss && (
            <button
              className="ghost-icon"
              type="button"
              onClick={onDismiss}
              title="Ocultar aviso"
              aria-label="Ocultar aviso"
              style={{ padding: "2px", height: "auto", minHeight: "unset" }}
            >
              <X size={14} />
            </button>
          )}
        </div>
      </div>
      <progress
        aria-label="Sincronización"
        max={100}
        value={syncBusy && !active ? undefined : syncProgress?.percent ?? (isComplete ? 100 : undefined)}
      />
      <small>{detailLabel}</small>
    </section>
  );
}
