import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useRef, useState, type Dispatch, type SetStateAction } from "react";
import { clearMediaCache, prepareMedia } from "./bridge/media";
import { readableError } from "./bridge/shared";
import { clearThumbnailCache } from "./FileThumbnail";
import type { CloudFile, DashboardData, FileKind } from "./types";

const PREVIEWABLE_KINDS = new Set<FileKind>([
  "image",
  "video",
  "audio",
  "pdf",
  "text",
  "code",
]);

export function canPreview(kind: FileKind): boolean {
  return PREVIEWABLE_KINDS.has(kind);
}

type UseMediaPreviewOptions = {
  connected: boolean;
  onRequireConnection: () => void;
  refreshDashboard: () => Promise<DashboardData | undefined>;
  setNotice: Dispatch<SetStateAction<string | null>>;
  formatBytes: (bytes: number) => string;
};

export function useMediaPreview({
  connected,
  onRequireConnection,
  refreshDashboard,
  setNotice,
  formatBytes,
}: UseMediaPreviewOptions) {
  const [mediaFile, setMediaFile] = useState<CloudFile | null>(null);
  const [mediaSrc, setMediaSrc] = useState<string | null>(null);
  const [previewText, setPreviewText] = useState<string | null>(null);
  const [mediaBusy, setMediaBusy] = useState(false);
  const [mediaError, setMediaError] = useState<string | null>(null);
  const mediaRequestRef = useRef(0);

  useEffect(() => () => {
    mediaRequestRef.current += 1;
  }, []);

  const closeMedia = () => {
    mediaRequestRef.current += 1;
    setMediaFile(null);
    setMediaSrc(null);
    setPreviewText(null);
    setMediaBusy(false);
    setMediaError(null);
  };

  const handleMedia = async (file: CloudFile) => {
    if (!canPreview(file.kind)) return;
    if (!connected) {
      onRequireConnection();
      return;
    }

    const request = ++mediaRequestRef.current;
    setMediaFile(file);
    setMediaSrc(null);
    setPreviewText(null);
    setMediaError(null);
    setMediaBusy(true);

    try {
      if ((file.kind === "text" || file.kind === "code") && file.sizeBytes > 2 * 1024 * 1024) {
        setMediaError("La vista previa de texto está limitada a 2 MB. Descarga el archivo para verlo completo.");
        return;
      }

      const ready = await prepareMedia(file.id);
      if (request !== mediaRequestRef.current) return;

      const source = convertFileSrc(ready.path);
      setMediaSrc(source);

      if (file.kind === "text" || file.kind === "code") {
        const response = await fetch(source);
        if (!response.ok) throw new Error("No se pudo leer la vista previa de texto");
        const text = await response.text();
        if (request !== mediaRequestRef.current) return;
        setPreviewText(text);
      }

      setNotice(
        ready.fromCache
          ? "Vista previa abierta desde la caché privada."
          : "Archivo preparado para vista previa en la caché privada de Nuvio.",
      );
      await refreshDashboard();
    } catch (error) {
      if (request !== mediaRequestRef.current) return;
      setNotice(`No se pudo preparar la vista previa: ${readableError(error)}`);
      setMediaFile(null);
    } finally {
      if (request === mediaRequestRef.current) setMediaBusy(false);
    }
  };

  const handleClearCache = async () => {
    try {
      const released = await clearMediaCache();
      clearThumbnailCache();
      setNotice(`Caché multimedia limpiada · ${formatBytes(released)} liberados.`);
      closeMedia();
      await refreshDashboard();
    } catch (error) {
      setNotice(readableError(error));
    }
  };

  return {
    mediaFile,
    mediaSrc,
    previewText,
    mediaBusy,
    mediaError,
    setMediaError,
    closeMedia,
    handleMedia,
    handleClearCache,
  };
}
