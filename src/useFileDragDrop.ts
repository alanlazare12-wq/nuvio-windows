import {
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type DragEvent,
  type PointerEvent,
  type SetStateAction,
} from "react";
import { moveFilesToFolder, moveFolder } from "./bridge/files";
import { readableError } from "./bridge/shared";
import type { CloudFile, CloudFolder, DashboardData, SectionKey } from "./types";

type TouchDragState = {
  pointerId: number;
  kind: "files" | "folder";
  ids: string[];
  folderId?: string;
  startX: number;
  startY: number;
  lastX: number;
  lastY: number;
  lastTarget: string | null;
  active: boolean;
  timer: number | null;
  element: HTMLElement;
};

type UseFileDragDropOptions = {
  dashboard: DashboardData | null;
  currentFolderId: string | null;
  selectedFiles: Set<string>;
  setSelectedFiles: Dispatch<SetStateAction<Set<string>>>;
  setCurrentFolderId: Dispatch<SetStateAction<string | null>>;
  setSection: Dispatch<SetStateAction<SectionKey>>;
  refreshDashboard: () => Promise<DashboardData | undefined>;
  setNotice: Dispatch<SetStateAction<string | null>>;
  onRequireConnection: () => void;
};

export function useFileDragDrop({
  dashboard,
  currentFolderId,
  selectedFiles,
  setSelectedFiles,
  setCurrentFolderId,
  setSection,
  refreshDashboard,
  setNotice,
  onRequireConnection,
}: UseFileDragDropOptions) {
  const [draggingFileIds, setDraggingFileIds] = useState<string[]>([]);
  const [draggingFolderId, setDraggingFolderId] = useState<string | null>(null);
  const [dragTarget, setDragTarget] = useState<string | null>(null);
  const [touchDragPosition, setTouchDragPosition] = useState<{ x: number; y: number } | null>(null);

  const draggingFileIdsRef = useRef<string[]>([]);
  const suppressClickUntilRef = useRef(0);
  const dragActiveRef = useRef(false);
  const touchDragRef = useRef<TouchDragState | null>(null);

  const dropTargetKeyAt = (x: number, y: number): string | null => {
    if (!Number.isFinite(x) || !Number.isFinite(y)) return null;
    return document.elementFromPoint(x, y)
      ?.closest<HTMLElement>("[data-nuvio-drop-folder]")
      ?.dataset.nuvioDropFolder ?? null;
  };

  const folderTargetIsValid = (movingFolderId: string, targetFolderId: string | null) => {
    if (!dashboard || movingFolderId === targetFolderId) return false;
    let cursor = targetFolderId;
    const seen = new Set<string>();
    while (cursor && !seen.has(cursor)) {
      if (cursor === movingFolderId) return false;
      seen.add(cursor);
      cursor = dashboard.folders.find((folder) => folder.id === cursor)?.parentId ?? null;
    }
    return true;
  };

  const dropTargetForTouchAt = (x: number, y: number, touch: TouchDragState): string | null => {
    const raw = dropTargetKeyAt(x, y);
    if (touch.kind !== "folder" || !touch.folderId || raw == null) return raw;
    const targetFolderId = raw === "__root__" ? null : raw;
    return folderTargetIsValid(touch.folderId, targetFolderId) ? raw : null;
  };

  const clearFileDrag = () => {
    draggingFileIdsRef.current = [];
    const touch = touchDragRef.current;
    if (touch?.active) suppressClickUntilRef.current = Date.now() + 400;
    if (touch?.element.hasPointerCapture(touch.pointerId)) {
      try { touch.element.releasePointerCapture(touch.pointerId); } catch { /* Optional. */ }
    }
    if (touch?.timer != null) window.clearTimeout(touch.timer);
    touchDragRef.current = null;
    dragActiveRef.current = false;
    setDraggingFileIds([]);
    setDraggingFolderId(null);
    setDragTarget(null);
    setTouchDragPosition(null);
  };

  useEffect(() => {
    const stopPan = (event: TouchEvent) => {
      if (touchDragRef.current?.active && event.cancelable) event.preventDefault();
    };
    const cancel = () => clearFileDrag();
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && touchDragRef.current) clearFileDrag();
    };
    document.addEventListener("touchmove", stopPan, { passive: false });
    window.addEventListener("keydown", escape);
    window.addEventListener("blur", cancel);
    return () => {
      document.removeEventListener("touchmove", stopPan);
      window.removeEventListener("blur", cancel);
      window.removeEventListener("keydown", escape);
      const touch = touchDragRef.current;
      if (touch?.timer != null) clearTimeout(touch.timer);
      dragActiveRef.current = false;
    };
  }, []);

  useEffect(() => {
    if ((!draggingFileIds.length && !draggingFolderId) || !touchDragPosition) return;
    let frame = 0;
    const scroll = () => {
      const touch = touchDragRef.current;
      if (!touch?.active) return;
      const content = document.querySelector<HTMLElement>(".content-scroll");
      const scroller = content && getComputedStyle(content).overflowY === "auto"
        ? content
        : document.scrollingElement;
      if (scroller) {
        const top = scroller === content ? Math.max(0, content!.getBoundingClientRect().top) : 0;
        const bottom = scroller === content ? Math.min(innerHeight, content!.getBoundingClientRect().bottom) : innerHeight;
        const delta = touch.lastY < top + 64 ? -14 : touch.lastY > bottom - 64 ? 14 : 0;
        if (delta) {
          scroller.scrollTop += delta;
          setDragTarget(dropTargetForTouchAt(touch.lastX, touch.lastY, touch));
        }
      }
      frame = requestAnimationFrame(scroll);
    };
    frame = requestAnimationFrame(scroll);
    return () => cancelAnimationFrame(frame);
  }, [draggingFileIds.length, draggingFolderId, Boolean(touchDragPosition), dashboard]);

  const dragIdsFor = (file: CloudFile) =>
    selectedFiles.has(file.id) && selectedFiles.size > 0 ? [...selectedFiles] : [file.id];

  const performFileDrop = async (ids: string[], targetFolderId: string | null) => {
    if (!ids.length || !dashboard) return;
    if (!dashboard.telegramConnected) {
      clearFileDrag();
      onRequireConnection();
      return;
    }
    const alreadyThere = ids.every((id) => {
      const file = dashboard.files.find((item) => item.id === id);
      return (file?.folderId ?? null) === targetFolderId;
    });
    if (alreadyThere) {
      clearFileDrag();
      setNotice("Los archivos ya están en esa ubicación.");
      return;
    }
    const destination = targetFolderId
      ? dashboard.folders.find((folder) => folder.id === targetFolderId)?.name ?? "la carpeta"
      : "Mi unidad";
    setNotice(`Moviendo ${ids.length} archivo${ids.length === 1 ? "" : "s"} a ${destination}…`);
    try {
      const changed = await moveFilesToFolder(ids, targetFolderId);
      setSelectedFiles(new Set());
      setNotice(`${changed} archivo${changed === 1 ? "" : "s"} movido${changed === 1 ? "" : "s"} a ${destination}.`);
      await refreshDashboard();
    } catch (error) {
      setNotice(readableError(error));
    } finally {
      clearFileDrag();
    }
  };

  const performFolderDrop = async (folderId: string, targetFolderId: string | null) => {
    if (!dashboard) return;
    const folder = dashboard.folders.find((item) => item.id === folderId && !item.trashed);
    if (!folder) return;
    if (!dashboard.telegramConnected) {
      clearFileDrag();
      onRequireConnection();
      return;
    }
    if ((folder.parentId ?? null) === targetFolderId) {
      clearFileDrag();
      setNotice(`La carpeta “${folder.name}” ya está en esa ubicación.`);
      return;
    }
    if (!folderTargetIsValid(folderId, targetFolderId)) {
      clearFileDrag();
      setNotice("No puedes mover una carpeta dentro de sí misma ni de una de sus subcarpetas.");
      return;
    }
    const destination = targetFolderId
      ? dashboard.folders.find((item) => item.id === targetFolderId)?.name ?? "la carpeta"
      : "Mi unidad";
    setNotice(`Moviendo la carpeta “${folder.name}” a ${destination}…`);
    try {
      await moveFolder(folderId, targetFolderId);
      setNotice(`Carpeta “${folder.name}” movida a ${destination}.`);
      await refreshDashboard();
    } catch (error) {
      setNotice(readableError(error));
    } finally {
      clearFileDrag();
    }
  };

  const desktopDragStart = (event: DragEvent<HTMLElement>, file: CloudFile) => {
    if (file.trashed) { event.preventDefault(); return; }
    const ids = dragIdsFor(file);
    suppressClickUntilRef.current = Date.now() + 400;
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData("application/x-nuvio-files", JSON.stringify(ids));
    event.dataTransfer.setData("text/plain", ids.join(","));
    draggingFileIdsRef.current = ids;
    setDraggingFileIds(ids);
    setDragTarget(null);
  };

  const desktopDrop = (event: DragEvent<HTMLElement>, targetFolderId: string | null) => {
    event.preventDefault();
    event.stopPropagation();
    let ids = draggingFileIdsRef.current.length ? draggingFileIdsRef.current : draggingFileIds;
    try {
      const raw = event.dataTransfer.getData("application/x-nuvio-files");
      if (raw) {
        const parsed: unknown = JSON.parse(raw);
        if (Array.isArray(parsed) && parsed.every((value) => typeof value === "string") && parsed.length > 0) {
          ids = parsed;
        }
      }
    } catch { /* Use in-memory drag state. */ }
    void performFileDrop(ids, targetFolderId);
  };

  const isInternalFileDrag = (event: DragEvent<HTMLElement>) =>
    draggingFileIdsRef.current.length > 0
    || Array.from(event.dataTransfer.types ?? []).includes("application/x-nuvio-files");

  const allowInternalDragCursor = (event: DragEvent<HTMLElement>) => {
    if (!isInternalFileDrag(event)) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
  };

  const cancelInternalDesktopDrop = (event: DragEvent<HTMLElement>) => {
    if (!isInternalFileDrag(event)) return;
    event.preventDefault();
    event.stopPropagation();
    clearFileDrag();
    setNotice("Movimiento cancelado.");
  };

  const currentFolderDropKey = currentFolderId ?? "__root__";
  const handleCurrentFolderDragOver = (event: DragEvent<HTMLElement>) => {
    if (!isInternalFileDrag(event)) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
    if (dragTarget !== currentFolderDropKey) setDragTarget(currentFolderDropKey);
  };
  const handleCurrentFolderDrop = (event: DragEvent<HTMLElement>) => {
    if (!isInternalFileDrag(event)) return;
    desktopDrop(event, currentFolderId);
  };

  const activateTouchDrag = (event: PointerEvent<HTMLElement>) => {
    const touch = touchDragRef.current;
    if (!touch || touch.pointerId !== event.pointerId || touch.active) return;
    touch.active = true;
    dragActiveRef.current = true;
    touch.timer = null;
    suppressClickUntilRef.current = Date.now() + 500;
    try { event.currentTarget.setPointerCapture(event.pointerId); } catch { /* Optional. */ }
    touch.lastX = event.clientX;
    touch.lastY = event.clientY;
    if (touch.kind === "folder") {
      setDraggingFolderId(touch.folderId ?? null);
      setDraggingFileIds([]);
      draggingFileIdsRef.current = [];
    } else {
      setDraggingFolderId(null);
      setDraggingFileIds(touch.ids);
      draggingFileIdsRef.current = touch.ids;
    }
    setTouchDragPosition({ x: event.clientX, y: event.clientY });
    const target = dropTargetForTouchAt(event.clientX, event.clientY, touch);
    touch.lastTarget = target;
    setDragTarget(target);
    try { navigator.vibrate?.(25); } catch { /* Optional. */ }
  };

  const touchDragStart = (event: PointerEvent<HTMLElement>, file: CloudFile) => {
    if (file.trashed) return;
    const interactive = (event.target as Element).closest('button,input,label,select,a,[role="button"]');
    if (interactive || !event.isPrimary) return;
    clearFileDrag();
    const ids = dragIdsFor(file);
    const state: TouchDragState = {
      pointerId: event.pointerId,
      kind: "files",
      ids,
      startX: event.clientX,
      startY: event.clientY,
      lastX: event.clientX,
      lastY: event.clientY,
      lastTarget: null,
      active: false,
      timer: null,
      element: event.currentTarget,
    };
    touchDragRef.current = state;

    if (event.pointerType === "mouse") {
      try { event.currentTarget.setPointerCapture(event.pointerId); } catch { /* Optional. */ }
    } else if (!selectedFiles.has(file.id)) {
      state.timer = window.setTimeout(() => {
        const current = touchDragRef.current;
        if (!current || current.pointerId !== event.pointerId || current.active) return;
        current.active = true;
        dragActiveRef.current = true;
        current.timer = null;
        suppressClickUntilRef.current = Date.now() + 500;
        try { current.element.setPointerCapture(current.pointerId); } catch { /* Optional. */ }
        current.lastX = current.startX;
        current.lastY = current.startY;
        setDraggingFileIds(current.ids);
        draggingFileIdsRef.current = current.ids;
        setTouchDragPosition({ x: current.startX, y: current.startY });
        const target = dropTargetKeyAt(current.startX, current.startY);
        current.lastTarget = target;
        setDragTarget(target);
        try { navigator.vibrate?.(25); } catch { /* Optional. */ }
      }, 260);
    }
  };

  const folderTouchDragStart = (event: PointerEvent<HTMLElement>, folder: CloudFolder) => {
    if (folder.trashed || !event.isPrimary) return;
    if ((event.target as Element).closest(".folder-actions button")) return;
    if (touchDragRef.current) clearFileDrag();
    const state: TouchDragState = {
      pointerId: event.pointerId,
      kind: "folder",
      ids: [],
      folderId: folder.id,
      startX: event.clientX,
      startY: event.clientY,
      lastX: event.clientX,
      lastY: event.clientY,
      lastTarget: null,
      active: false,
      timer: null,
      element: event.currentTarget,
    };
    touchDragRef.current = state;

    if (event.pointerType === "mouse") {
      try { event.currentTarget.setPointerCapture(event.pointerId); } catch { /* Optional. */ }
    } else {
      state.timer = window.setTimeout(() => {
        const current = touchDragRef.current;
        if (!current || current.pointerId !== event.pointerId || current.active || current.kind !== "folder") return;
        current.active = true;
        dragActiveRef.current = true;
        current.timer = null;
        suppressClickUntilRef.current = Date.now() + 500;
        try { current.element.setPointerCapture(current.pointerId); } catch { /* Optional. */ }
        current.lastX = current.startX;
        current.lastY = current.startY;
        setDraggingFolderId(current.folderId ?? null);
        setDraggingFileIds([]);
        draggingFileIdsRef.current = [];
        setTouchDragPosition({ x: current.startX, y: current.startY });
        const target = dropTargetForTouchAt(current.startX, current.startY, current);
        current.lastTarget = target;
        setDragTarget(target);
        try { navigator.vibrate?.(25); } catch { /* Optional. */ }
      }, 260);
    }
  };

  const touchDragMove = (event: PointerEvent<HTMLElement>) => {
    const touch = touchDragRef.current;
    if (!touch || touch.pointerId !== event.pointerId) return;
    if (!touch.active) {
      const distance = Math.hypot(event.clientX - touch.startX, event.clientY - touch.startY);
      const isMouse = event.pointerType === "mouse";
      if (isMouse && distance > 4) {
        activateTouchDrag(event);
      } else if (!isMouse && distance > 6 && touch.timer == null) {
        activateTouchDrag(event);
      } else if (!isMouse && distance > 12 && touch.timer != null) {
        window.clearTimeout(touch.timer);
        touch.timer = null;
        touchDragRef.current = null;
      }
      if (!touch.active) return;
    }
    event.preventDefault();
    touch.lastX = event.clientX;
    touch.lastY = event.clientY;
    setTouchDragPosition({ x: event.clientX, y: event.clientY });
    const target = dropTargetForTouchAt(event.clientX, event.clientY, touch);
    touch.lastTarget = target;
    setDragTarget(target);
  };

  const touchDragEnd = (event: PointerEvent<HTMLElement>) => {
    const touch = touchDragRef.current;
    if (!touch || touch.pointerId !== event.pointerId) return;
    if (touch.timer != null) window.clearTimeout(touch.timer);
    try { event.currentTarget.releasePointerCapture(event.pointerId); } catch { /* Optional. */ }

    if (!touch.active) {
      const folderId = touch.kind === "folder" ? touch.folderId : null;
      touchDragRef.current = null;
      dragActiveRef.current = false;
      if (folderId) {
        setCurrentFolderId(folderId);
        setSelectedFiles(new Set());
        setSection("files");
      }
      return;
    }

    event.preventDefault();
    const key = dropTargetForTouchAt(event.clientX, event.clientY, touch);
    const ids = touch.ids;
    const folderId = touch.folderId;
    const kind = touch.kind;
    clearFileDrag();
    if (key == null) {
      setNotice("Movimiento cancelado.");
      return;
    }
    try { navigator.vibrate?.(30); } catch { /* Optional. */ }
    const targetFolderId = key === "__root__" ? null : key;
    if (kind === "folder" && folderId) {
      void performFolderDrop(folderId, targetFolderId);
    } else {
      void performFileDrop(ids, targetFolderId);
    }
  };

  const toggleSelection = (id: string) => {
    if (Date.now() < suppressClickUntilRef.current) return;
    setSelectedFiles((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  return {
    draggingFileIds,
    draggingFolderId,
    dragTarget,
    setDragTarget,
    touchDragPosition,
    dragActiveRef,
    clearFileDrag,
    desktopDragStart,
    desktopDrop,
    isInternalFileDrag,
    allowInternalDragCursor,
    cancelInternalDesktopDrop,
    currentFolderDropKey,
    handleCurrentFolderDragOver,
    handleCurrentFolderDrop,
    touchDragStart,
    folderTouchDragStart,
    touchDragMove,
    touchDragEnd,
    toggleSelection,
  };
}
