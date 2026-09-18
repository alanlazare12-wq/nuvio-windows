import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useEffect, useRef, useState } from "react";
import type { DashboardData, SectionKey } from "./types";

type UseNativeFileDropOptions = {
  dashboard: DashboardData | null;
  section: SectionKey;
  currentFolderId: string | null;
  handleExternalDrop: (paths: string[], targetFolderId: string | null) => Promise<void>;
};

export function useNativeFileDrop({
  dashboard,
  section,
  currentFolderId,
  handleExternalDrop,
}: UseNativeFileDropOptions) {
  const [externalDragActive, setExternalDragActive] = useState(false);
  const [externalDragTargetName, setExternalDragTargetName] = useState("Mi unidad");

  const dashboardRef = useRef(dashboard);
  const sectionRef = useRef(section);
  const currentFolderIdRef = useRef(currentFolderId);
  const externalDropRef = useRef(handleExternalDrop);
  dashboardRef.current = dashboard;
  sectionRef.current = section;
  currentFolderIdRef.current = currentFolderId;
  externalDropRef.current = handleExternalDrop;

  const dropTargetKeyAt = (x: number, y: number): string | null => {
    if (!Number.isFinite(x) || !Number.isFinite(y)) return null;
    return document.elementFromPoint(x, y)
      ?.closest<HTMLElement>("[data-nuvio-drop-folder]")
      ?.dataset.nuvioDropFolder ?? null;
  };

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;
    let nativeDragActive = false;

    try {
      const webview = getCurrentWebview();
      if (webview && typeof webview.onDragDropEvent === "function") {
        void webview.onDragDropEvent((event) => {
          if (disposed) return;

          if (event.payload.type === "enter") {
            const hasPaths = Array.isArray(event.payload.paths) && event.payload.paths.length > 0;
            if (!hasPaths) return;
            nativeDragActive = true;
            setExternalDragActive(true);
          } else if (event.payload.type === "over") {
            if (!nativeDragActive) return;
          }

          if (event.payload.type === "enter" || event.payload.type === "over") {
            const pos = event.payload.position;
            if (!pos) return;

            const dpr = window.devicePixelRatio || 1;
            const targetKey = dropTargetKeyAt(pos.x / dpr, pos.y / dpr);
            const currentDashboard = dashboardRef.current;
            const currentId = currentFolderIdRef.current;

            if (targetKey && targetKey !== "__root__") {
              const folder = currentDashboard?.folders.find((item) => item.id === targetKey);
              setExternalDragTargetName(folder?.name ?? "esta carpeta");
            } else if (currentId) {
              const folder = currentDashboard?.folders.find((item) => item.id === currentId);
              setExternalDragTargetName(folder?.name ?? "Mi unidad");
            } else {
              setExternalDragTargetName("Mi unidad");
            }
          } else if (event.payload.type === "leave") {
            nativeDragActive = false;
            setExternalDragActive(false);
          } else if (event.payload.type === "drop") {
            nativeDragActive = false;
            setExternalDragActive(false);

            const paths = event.payload.paths;
            if (!paths?.length) return;

            const pos = event.payload.position;
            const dpr = window.devicePixelRatio || 1;
            const targetKey = dropTargetKeyAt(pos.x / dpr, pos.y / dpr);
            const targetFolderId = targetKey === "__root__"
              ? null
              : targetKey || (
                sectionRef.current === "files"
                  ? currentFolderIdRef.current
                  : null
              );
            void externalDropRef.current(paths, targetFolderId);
          }
        }).then((fn) => {
          if (disposed) fn();
          else unlisten = fn;
        }).catch(() => {
          // Native events are unavailable in browser previews.
        });
      }
    } catch {
      // Non-desktop platform or native drag/drop unavailable.
    }

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  return {
    externalDragActive,
    externalDragTargetName,
  };
}
