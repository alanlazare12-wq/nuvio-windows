import { Square, X } from "lucide-react";
import type { SyncProgress } from "../types";

export interface SyncProgressPanelProps {
  syncProgress?: SyncProgress | null;
  syncBusy: boolean;
  cancelBusy?: boolean;
  onCancel?: () => void;
  onDismiss?: () => void;
}

export function SyncProgressPanel({
  syncProgress,
  syncBusy,
  cancelBusy = false,
  onCancel,
  onDismiss,
}: SyncProgressPanelProps) {
  if (!syncProgress?.phase && !syncBusy) return null;

  const active = Boolean(syncProgress?.active);
  const phase = syncProgress?.phase;
  const isFolders = phase === "folders";
  const isFiles = phase === "files";
  const isApplying = phase === "applying";
  const isPublishing = phase === "publishing";
  const isCancelled = phase === "cancelled";
  const isComplete = phase === "complete";
  const hasError = Boolean(syncProgress?.error);

  const title = active
    ? isFolders
      ? "Cargando carpetas…"
      : isFiles
      ? "Sincronizando archivos…"
      : isApplying
      ? "Actualizando catálogo…"
      : isPublishing
      ? "Finalizando catálogo…"
      : "Sincronizando con Telegram…"
    : syncBusy
    ? "Iniciando sincronización…"
    : isCancelled
    ? "Sincronización detenida"
    : hasError
    ? "Sincronización interrumpida"
    : "Sincronización completada";

  const percentLabel = active
    ? syncProgress?.percent == null
      ? "Calculando…"
      : `${syncProgress.percent}% aprox.`
    : syncBusy
    ? "Calculando…"
    : isCancelled
    ? "Detenida"
    : hasError
    ? "Pendiente de reintentar"
    : "100%";

  const detailLabel =
    syncProgress?.error ||
    (active
      ? isFolders
        ? "Preparando estructura de carpetas antes de mostrar archivos…"
        : isApplying
        ? "Aplicando los cambios encontrados al catálogo local…"
        : isPublishing
        ? "Guardando el checkpoint remoto del catálogo…"
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
      : isCancelled
      ? "Puedes volver a sincronizar cuando quieras; no se avanzó ningún checkpoint incompleto."
      : "Catálogo actualizado");

  return (
    <section className="sync-progress-panel" aria-label="Progreso de sincronización">
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
        <strong>{title}</strong>
        <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
          <span>{percentLabel}</span>
          {active && onCancel && (
            <button
              className="secondary-button compact"
              type="button"
              onClick={onCancel}
              disabled={cancelBusy}
              title="Detener sincronización"
              aria-label="Detener sincronización"
            >
              <Square size={12} fill="currentColor" /> {cancelBusy ? "Deteniendo…" : "Detener"}
            </button>
          )}
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
