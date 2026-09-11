import {
  AlertCircle,
  AlertTriangle,
  Archive,
  ArrowDownToLine,
  ArrowUpFromLine,
  Check,
  CheckCircle2,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Clock3,
  Cloud,
  Copy,
  Download,
  Eye,
  File,
  FileAudio,
  FileImage,
  FileText,
  FileVideo,
  Files,
  Filter,
  Folder,
  FolderOpen,
  FolderPlus,
  FolderUp,
  Grid2X2,
  HardDrive,
  Home,
  List,
  Menu,
  Moon,
  Move,
  Pause,
  Pencil,
  Play,
  Plus,
  RefreshCw,
  RotateCcw,
  Search,
  Settings,
  SlidersHorizontal,
  Sparkles,
  Star,
  Sun,
  Trash2,
  Upload,
  X,
  Zap,
} from "lucide-react";
import { memo, useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { getDocument, GlobalWorkerOptions } from "pdfjs-dist";
import pdfWorker from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import {
  backgroundApp,
  listenMobileBack,
  cancelTransfer,
  clearMediaCache,
  clearTransferHistory,
  configureTelegram,
  createFolder,
  deleteFolder,
  deleteFilesPermanently,
  emptyTrash,
  exportDiagnostics,
  forgetTelegramSession,
  getTelegramAuthState,
  inspectDroppedPaths,
  loadDashboard,
  logOutTelegram,
  moveFilesToFolder,
  moveFolder,
  pauseQueue,
  pauseTransfer,
  prepareMedia,
  prepareUploadBatch,
  prepareZipUploads,
  prepareUploadItemsBatch,
  queueDownloads,
  readableError,
  registerTelegramUser,
  requestTelegramQr,
  renameFolder,
  resendTelegramCode,
  resumeQueue,
  resumeTransfer,
  retryTransfer,
  scanDirectoryForUpload,
  selectDownloadDirectory,
  selectFilesForUpload,
  selectFolderForUpload,
  setFavorite,
  setTrashed,
  setTrashedMany,
  submitTelegramCode,
  submitTelegramEmail,
  submitTelegramEmailCode,
  submitTelegramPassword,
  submitTelegramPhone,
  syncFiles,
  updateSyncNotification,
  updateUploadNotification,
  updateSetting,
} from "./bridge";
import type {
  CloudFile,
  CloudFolder,
  DashboardData,
  DirectoryUploadPlan,
  FileFilter,
  FileKind,
  SectionKey,
  SkippedUploadItem,
  SortKey,
  TelegramAuthSnapshot,
  TransferFilter,
  TransferJob,
} from "./types";
import "./App.css";
import { Dialog } from "./Dialog";
import { FileThumbnail, clearThumbnailCache } from "./FileThumbnail";

GlobalWorkerOptions.workerSrc = pdfWorker;

const navItems: Array<{ key: SectionKey; label: string; icon: typeof Home }> = [
  { key: "home", label: "Inicio", icon: Home },
  { key: "files", label: "Mis archivos", icon: Files },
  { key: "favorites", label: "Favoritos", icon: Star },
  { key: "recent", label: "Recientes", icon: Clock3 },
  { key: "history", label: "Historial", icon: Clock3 },
  { key: "trash", label: "Papelera", icon: Trash2 },
];

const filterItems: Array<{ key: FileFilter; label: string }> = [
  { key: "all", label: "Todos" },
  { key: "image", label: "Imágenes" },
  { key: "video", label: "Videos" },
  { key: "audio", label: "Audio" },
  { key: "pdf", label: "PDF" },
  { key: "document", label: "Documentos" },
  { key: "spreadsheet", label: "Hojas de cálculo" },
  { key: "presentation", label: "Presentaciones" },
  { key: "text", label: "Texto" },
  { key: "code", label: "Código" },
  { key: "archive", label: "Comprimidos" },
  { key: "ebook", label: "E-books" },
  { key: "database", label: "Bases de datos" },
  { key: "font", label: "Fuentes" },
  { key: "package", label: "Paquetes" },
  { key: "model", label: "3D / CAD" },
  { key: "other", label: "Otros" },
];

const tagOptions = ["Trabajo", "Personal", "Fotos", "Proyectos"];

function initialDarkMode(): boolean {
  try {
    const stored = localStorage.getItem("nuvio-theme");
    if (stored === "dark" || stored === "light") return stored === "dark";
  } catch { /* Storage may be unavailable in a restricted webview. */ }
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

function fileName(file: CloudFile): string {
  return file.extension ? `${file.name}.${file.extension}` : file.name;
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const value = bytes / 1024 ** index;
  return `${value >= 10 || index < 2 ? value.toFixed(index === 0 ? 0 : 1) : value.toFixed(2)} ${units[index]}`;
}

function formatSpeed(bytesPerSecond: number): string {
  return bytesPerSecond > 0 ? `${formatBytes(bytesPerSecond)}/s` : "—";
}

function formatEta(seconds?: number | null, calculating = false): string {
  if (seconds == null || !Number.isFinite(seconds)) return calculating ? "Calculando tiempo restante…" : "—";
  if (seconds <= 0) return "Menos de 1 s";
  const rounded = Math.ceil(seconds);
  const hours = Math.floor(rounded / 3600);
  const minutes = Math.floor((rounded % 3600) / 60);
  const secs = rounded % 60;
  if (hours > 0) return `${hours} h ${minutes} min`;
  if (minutes > 0) return `${minutes} min ${secs} s`;
  return `${secs} s`;
}

function relativeDate(value: string): string {
  const date = new Date(value);
  const diff = Date.now() - date.getTime();
  const hours = Math.max(0, Math.floor(diff / 3_600_000));
  if (hours < 1) return "Hace unos minutos";
  if (hours < 24) return `Hace ${hours} h`;
  const days = Math.floor(hours / 24);
  if (days === 1) return "Ayer";
  if (days < 7) return `Hace ${days} días`;
  return new Intl.DateTimeFormat("es-MX", { day: "numeric", month: "short" }).format(date);
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

function kindIcon(kind: FileKind) {
  switch (kind) {
    case "image": return FileImage;
    case "video": return FileVideo;
    case "audio": return FileAudio;
    case "archive": return Archive;
    case "pdf":
    case "document":
    case "spreadsheet":
    case "presentation":
    case "text":
    case "code":
    case "ebook": return FileText;
    default: return File;
  }
}

function kindLabel(kind: FileKind): string {
  return ({
    image: "Imagen",
    video: "Video",
    audio: "Audio",
    pdf: "PDF",
    document: "Documento",
    spreadsheet: "Hoja de cálculo",
    presentation: "Presentación",
    text: "Texto",
    code: "Código",
    archive: "Comprimido",
    ebook: "E-book",
    database: "Base de datos",
    font: "Fuente",
    package: "Paquete / instalador",
    model: "3D / CAD",
    other: "Archivo",
  } as Record<FileKind, string>)[kind];
}

function canPreview(kind: FileKind): boolean {
  return ["image", "video", "audio", "pdf", "text", "code"].includes(kind);
}

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
function isTransferPending(job: TransferJob) {
  return !["completed", "failed", "duplicate", "cancelled"].includes(job.status);
}

function App() {
  const [dashboard, setDashboard] = useState<DashboardData | null>(null);
  const [section, setSection] = useState<SectionKey>("home");
  const [currentFolderId, setCurrentFolderId] = useState<string | null>(null);
  const [folderEditor, setFolderEditor] = useState<{ mode: "create" | "rename"; folder?: CloudFolder } | null>(null);
  const [moveDialog, setMoveDialog] = useState<{ kind: "files"; ids: string[] } | { kind: "folder"; folder: CloudFolder } | null>(null);
  const [query, setQuery] = useState("");
  const [fileFilter, setFileFilter] = useState<FileFilter>("all");
  const [sort, setSort] = useState<SortKey>("recent");
  const [view, setView] = useState<"grid" | "list">("grid");
  const [selectedTag, setSelectedTag] = useState<string | null>(null);
  const [filterPanel, setFilterPanel] = useState(false);
  const [connectModal, setConnectModal] = useState(false);
  const [mobileMenu, setMobileMenu] = useState(false);
  const [darkMode, setDarkMode] = useState(initialDarkMode);
  const [uploadBusy, setUploadBusy] = useState(false);
  const [zipBeforeUpload, setZipBeforeUpload] = useState(false);
  const [zipProgress, setZipProgress] = useState<{ processed: number; total: number; name: string } | null>(null);
  const [syncBusy, setSyncBusy] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [appNotice, setAppNotice] = useState<string | null>(null);
  const [skippedFiles, setSkippedFiles] = useState<SkippedUploadItem[]>([]);
  const [showSkippedModal, setShowSkippedModal] = useState(false);
  const [selectedFiles, setSelectedFiles] = useState<Set<string>>(() => new Set());
  const [transferFilter, setTransferFilter] = useState<TransferFilter>("all");
  const [mediaFile, setMediaFile] = useState<CloudFile | null>(null);
  const [mediaSrc, setMediaSrc] = useState<string | null>(null);
  const [previewText, setPreviewText] = useState<string | null>(null);
  const [mediaBusy, setMediaBusy] = useState(false);
  const [mediaError, setMediaError] = useState<string | null>(null);
  const [draggingFileIds, setDraggingFileIds] = useState<string[]>([]);
  const draggingFileIdsRef = useRef<string[]>([]);
  const suppressClickUntilRef = useRef(0);
  const [dragTarget, setDragTarget] = useState<string | null>(null);
  const [externalDragActive, setExternalDragActive] = useState(false);
  const [externalDragTargetName, setExternalDragTargetName] = useState<string>("Mi unidad");
  const [touchDragPosition, setTouchDragPosition] = useState<{ x: number; y: number } | null>(null);
  const touchDragRef = useRef<{
    pointerId: number;
    ids: string[];
    startX: number;
    startY: number;
    lastX: number;
    lastY: number;
    lastTarget: string | null;
    active: boolean;
    timer: number | null;
    element: HTMLElement;
  } | null>(null);
  useEffect(() => {
    // A non-passive listener keeps a long press from turning into a browser pan.
    const stopPan = (event: TouchEvent) => {
      if (touchDragRef.current?.active && event.cancelable) event.preventDefault();
    };
    const cancel = () => clearFileDrag();
    document.addEventListener("touchmove", stopPan, { passive: false });
    const escape = (event: KeyboardEvent) => { if (event.key === "Escape" && touchDragRef.current) clearFileDrag(); };
    window.addEventListener("keydown", escape);
    window.addEventListener("blur", cancel);
    return () => {
      document.removeEventListener("touchmove", stopPan);
      window.removeEventListener("blur", cancel);
      window.removeEventListener("keydown", escape);
      const touch = touchDragRef.current;
      if (touch?.timer != null) clearTimeout(touch.timer);
    };
  }, []);
  useEffect(() => { if (!dashboard?.telegramConnected) clearThumbnailCache(); }, [dashboard?.telegramConnected]);
  useEffect(() => {
    if (!draggingFileIds.length || !touchDragPosition) return;
    let frame = 0;
    const scroll = () => {
      const touch = touchDragRef.current;
      if (!touch?.active) return;
      const content = document.querySelector<HTMLElement>(".content-scroll");
      const scroller = content && getComputedStyle(content).overflowY === "auto" ? content : document.scrollingElement;
      if (scroller) {
        const top = scroller === content ? Math.max(0, content!.getBoundingClientRect().top) : 0;
        const bottom = scroller === content ? Math.min(innerHeight, content!.getBoundingClientRect().bottom) : innerHeight;
        const delta = touch.lastY < top + 64 ? -14 : touch.lastY > bottom - 64 ? 14 : 0;
        if (delta) {
          scroller.scrollTop += delta;
          setDragTarget(dropTargetKeyAt(touch.lastX, touch.lastY));
        }
      }
      frame = requestAnimationFrame(scroll);
    };
    frame = requestAnimationFrame(scroll);
    return () => cancelAnimationFrame(frame);
  }, [draggingFileIds.length, !!touchDragPosition]);
  const filterTabsRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const fileSearchInputRef = useRef<HTMLInputElement>(null);
  const mediaRequestRef = useRef(0);
  const dashboardRequestRef = useRef(0);
  const downloadBusyRef = useRef(false);
  const previousPendingRef = useRef<number | null>(null);

  const backHandlerRef = useRef<() => void>(() => {});
  backHandlerRef.current = () => {
    if (document.querySelector('[role="dialog"]')) {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    } else if (mobileMenu) setMobileMenu(false);
    else if (selectedFiles.size) setSelectedFiles(new Set());
    else if (filterPanel) setFilterPanel(false);
    else if (query) setQuery("");
    else if (section === "files" && currentFolderId) {
      const current = dashboard?.folders.find((folder) => folder.id === currentFolderId);
      setCurrentFolderId(current?.parentId ?? null);
    }
    else if (section !== "home") setSection("home");
    else void backgroundApp().catch(error => setAppNotice(readableError(error)));
  };
  useEffect(() => {
    let disposed = false;
    let stop: (() => void) | undefined;
    void listenMobileBack(() => backHandlerRef.current()).then(unlisten => {
      if (disposed) unlisten(); else stop = unlisten;
    }).catch(error => setAppNotice(readableError(error)));
    return () => { disposed = true; stop?.(); };
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        const target = searchInputRef.current ?? fileSearchInputRef.current;
        target?.focus();
        target?.select();
      } else if (event.key === "/" && !["INPUT", "TEXTAREA", "SELECT"].includes((document.activeElement?.tagName ?? ""))) {
        event.preventDefault();
        const target = searchInputRef.current ?? fileSearchInputRef.current;
        target?.focus();
        target?.select();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  useEffect(() => {
    if (!appNotice) return;
    const lower = appNotice.toLowerCase();
    if (lower.includes("error") || lower.includes("fallid") || lower.includes("no se pudo")) return;
    const timer = window.setTimeout(() => setAppNotice(null), 5000);
    return () => window.clearTimeout(timer);
  }, [appNotice]);

  const refreshDashboard = async () => {
    const request = ++dashboardRequestRef.current;
    try {
      const next = await loadDashboard();
      if (request !== dashboardRequestRef.current) return;
      setDashboard(next);
      setLoadError(null);
      return next;
    } catch (error) {
      if (request === dashboardRequestRef.current) setLoadError(readableError(error));
    }
  };

  useEffect(() => {
    let disposed = false;
    let timer = 0;
    const poll = async () => {
      if (disposed) return;
      if (touchDragRef.current?.active) {
        timer = window.setTimeout(poll, 800);
        return;
      }
      const next = await refreshDashboard();
      const active = next?.syncProgress?.active || (next?.queueSummary.pending ?? 0) > 0;
      if (!disposed) timer = window.setTimeout(poll, document.hidden ? 10000 : active ? 1200 : 3000);
    };
    void poll();
    return () => { disposed = true; dashboardRequestRef.current++; mediaRequestRef.current++; window.clearTimeout(timer); };
  }, []);

  const previousSyncActiveRef = useRef(false);
  const previousSyncKeyRef = useRef<string>("");
  useEffect(() => {
    if (!dashboard) return;
    const sync = dashboard.syncProgress;
    const active = Boolean(sync?.active);
    const wasActive = previousSyncActiveRef.current;
    if (active) {
      previousSyncActiveRef.current = true;
      const key = `${sync?.percent}:${sync?.scanned}:${sync?.phase}`;
      if (key !== previousSyncKeyRef.current) {
        previousSyncKeyRef.current = key;
        void updateSyncNotification({
          active: true,
          percent: sync?.percent,
          scanned: sync?.scanned ?? 0,
          total: sync?.total,
          phase: sync?.phase,
          error: sync?.error,
        });
      }
    } else if (wasActive) {
      previousSyncActiveRef.current = false;
      previousSyncKeyRef.current = "";
      void updateSyncNotification({
        active: false,
        percent: 100,
        scanned: sync?.scanned ?? 0,
        total: sync?.total,
        phase: sync?.phase ?? "complete",
        error: sync?.error,
      });
    }
  }, [dashboard?.syncProgress?.active, dashboard?.syncProgress?.percent, dashboard?.syncProgress?.scanned, dashboard?.syncProgress?.phase, dashboard?.syncProgress?.error]);

  const previousUploadActiveRef = useRef(false);
  const previousUploadKeyRef = useRef<string>("");
  useEffect(() => {
    if (!dashboard) return;
    const q = dashboard.queueSummary;
    const activeUploads = dashboard.transfers.filter(
      (t) => t.direction === "upload" && isTransferPending(t)
    );
    const hasActive = activeUploads.length > 0 || (q.pending > 0 && q.active > 0);
    const wasActive = previousUploadActiveRef.current;

    if (hasActive) {
      previousUploadActiveRef.current = true;
      const currentFileName = activeUploads[0]?.fileName ?? null;
      const pct = q.totalBytes > 0 ? Math.round((q.processedBytes / q.totalBytes) * 100) : null;
      const key = `${pct}:${q.completed}:${q.pending}:${currentFileName}:${Math.round(q.speedBps / 50000)}`;
      if (key !== previousUploadKeyRef.current) {
        previousUploadKeyRef.current = key;
        void updateUploadNotification({
          active: true,
          total: q.total,
          completed: q.completed,
          pending: q.pending,
          failed: q.failed,
          percent: pct,
          processedBytes: q.processedBytes,
          totalBytes: q.totalBytes,
          speedBps: q.speedBps,
          currentFileName,
        });
      }
    } else if (wasActive) {
      previousUploadActiveRef.current = false;
      previousUploadKeyRef.current = "";
      void updateUploadNotification({
        active: false,
        total: q.total,
        completed: q.completed,
        pending: 0,
        failed: q.failed,
        percent: 100,
        processedBytes: q.totalBytes,
        totalBytes: q.totalBytes,
        speedBps: 0,
      });
    }
  }, [dashboard?.queueSummary?.pending, dashboard?.queueSummary?.completed, dashboard?.queueSummary?.processedBytes, dashboard?.queueSummary?.speedBps, dashboard?.transfers]);

  useEffect(() => {
    document.documentElement.dataset.theme = darkMode ? "dark" : "light";
    try { localStorage.setItem("nuvio-theme", darkMode ? "dark" : "light"); } catch { /* The theme still works for this session. */ }
  }, [darkMode]);

  useEffect(() => {
    if (!dashboard) return;
    const pending = dashboard.queueSummary.pending;
    const previous = previousPendingRef.current;
    previousPendingRef.current = pending;
    if (previous == null || previous <= 0 || pending !== 0) return;

    const failed = dashboard.queueSummary.failed;
    setAppNotice(
      failed > 0
        ? `La cola terminó con ${failed} transferencia${failed === 1 ? "" : "es"} con error.`
        : `Cola completada · ${dashboard.fileCount} archivo${dashboard.fileCount === 1 ? "" : "s"} sincronizado${dashboard.fileCount === 1 ? "" : "s"}.`,
    );
    void (async () => {
      try {
        let granted = await isPermissionGranted();
        if (!granted) granted = (await requestPermission()) === "granted";
        if (granted) {
          sendNotification({
            title: "Nuvio · Cola finalizada",
            body: failed > 0
              ? `La cola terminó con ${failed} transferencia${failed === 1 ? "" : "es"} fallida${failed === 1 ? "" : "s"}.`
              : `${dashboard.fileCount} archivo${dashboard.fileCount === 1 ? "" : "s"} sincronizado${dashboard.fileCount === 1 ? "" : "s"}.`,
          });
        }
      } catch {
        // Las notificaciones son opcionales y nunca deben bloquear transferencias.
      }
    })();
  }, [dashboard?.queueSummary.pending, dashboard?.queueSummary.failed, dashboard?.queueSummary.total, dashboard?.fileCount, dashboard?.telegramConnected]);

  const currentFolder = dashboard?.folders.find((folder) => folder.id === currentFolderId && !folder.trashed) ?? null;
  const folderBreadcrumbs = useMemo(() => {
    if (!dashboard || !currentFolderId) return [] as CloudFolder[];
    const result: CloudFolder[] = [];
    const seen = new Set<string>();
    let cursor: string | null = currentFolderId;
    while (cursor && !seen.has(cursor)) {
      seen.add(cursor);
      const folder = dashboard.folders.find((item) => item.id === cursor && !item.trashed);
      if (!folder) break;
      result.unshift(folder);
      cursor = folder.parentId ?? null;
    }
    return result;
  }, [dashboard, currentFolderId]);
  const visibleFolders = useMemo(() => {
    if (!dashboard || section !== "files") return [] as CloudFolder[];
    const needle = query.trim().toLowerCase();
    return dashboard.folders
      .filter((folder) => !folder.trashed && (needle ? true : (folder.parentId ?? null) === currentFolderId))
      .filter((folder) => !needle || folder.name.toLowerCase().includes(needle))
      .sort((a, b) => a.name.localeCompare(b.name, "es", { numeric: true, sensitivity: "base" }));
  }, [dashboard, currentFolderId, query, section]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    try {
      const webview = getCurrentWebview();
      if (webview && typeof webview.onDragDropEvent === "function") {
        void webview.onDragDropEvent((event) => {
          if (event.payload.type === "enter") {
            const hasPaths = Array.isArray(event.payload.paths) && event.payload.paths.length > 0;
            if (hasPaths) {
              setExternalDragActive(true);
            } else {
              return;
            }
          } else if (event.payload.type === "over") {
            if (!externalDragActive) return;
          }
          if (event.payload.type === "enter" || event.payload.type === "over") {
            const pos = event.payload.position;
            if (pos) {
              const dpr = window.devicePixelRatio || 1;
              const targetKey = dropTargetKeyAt(pos.x / dpr, pos.y / dpr);
              if (targetKey && targetKey !== "__root__") {
                const folder = dashboard?.folders.find((f) => f.id === targetKey);
                setExternalDragTargetName(folder ? folder.name : "esta carpeta");
              } else if (currentFolder) {
                setExternalDragTargetName(currentFolder.name);
              } else {
                setExternalDragTargetName("Mi unidad");
              }
            }
          } else if (event.payload.type === "leave") {
            setExternalDragActive(false);
          } else if (event.payload.type === "drop") {
            setExternalDragActive(false);
            const paths = event.payload.paths;
            if (paths && paths.length > 0) {
              const pos = event.payload.position;
              const dpr = window.devicePixelRatio || 1;
              const targetKey = dropTargetKeyAt(pos.x / dpr, pos.y / dpr);
              const targetFolderId = targetKey === "__root__"
                ? null
                : (targetKey || (section === "files" ? currentFolderId : null));
              void handleExternalDrop(paths, targetFolderId);
            }
          }
        }).then((fn) => { unlisten = fn; });
      }
    } catch {
      // Plataforma no de escritorio o sin soporte nativo de drag & drop
    }
    return () => {
      if (unlisten) unlisten();
    };
  }, [dashboard?.folders, currentFolder, currentFolderId, section, zipBeforeUpload]);

  const files = useMemo(() => {
    if (!dashboard) return [];
    const needle = query.trim().toLowerCase();
    let output = dashboard.files.filter((item) => {
      if (section === "favorites" && !item.favorite) return false;
      if (section === "trash" && !item.trashed) return false;
      if (section !== "trash" && item.trashed) return false;
      if (fileFilter !== "all" && item.kind !== fileFilter) return false;
      if (selectedTag && !item.tags.includes(selectedTag)) return false;
      if (section === "files") {
        if (!needle && (item.folderId ?? null) !== currentFolderId) return false;
      }
      if (!needle) return true;
      const normalized = `${item.name} ${item.extension} ${item.folder} ${item.tags.join(" ")}`.toLowerCase();
      return normalized.includes(needle);
    });
    if (sort === "name") {
      output.sort((a, b) => a.name.localeCompare(b.name, "es", { numeric: true, sensitivity: "base" }));
    } else if (sort === "size") {
      output.sort((a, b) => b.sizeBytes - a.sizeBytes);
    } else if (sort === "oldest") {
      output.sort((a, b) => (Date.parse(a.updatedAt) || 0) - (Date.parse(b.updatedAt) || 0));
    }
    // Nota: para sort === "recent", dashboard.files ya viene ordenado por updated_at DESC desde SQLite
    return section === "home" ? output.slice(0, 12) : output;
  }, [dashboard, currentFolderId, fileFilter, query, section, selectedTag, sort]);

  const [visibleCount, setVisibleCount] = useState(80);
  const loadMoreSentinelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setVisibleCount(80);
  }, [section, currentFolderId, fileFilter, query, sort, selectedTag]);

  useEffect(() => {
    const el = loadMoreSentinelRef.current;
    if (!el) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          setVisibleCount((prev) => Math.min(prev + 80, files.length));
        }
      },
      { rootMargin: "400px" }
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [files.length, visibleCount]);

  const visibleFiles = useMemo(() => {
    return files.slice(0, visibleCount);
  }, [files, visibleCount]);

  useEffect(() => {
    if (currentFolderId && dashboard && !dashboard.folders.some((folder) => folder.id === currentFolderId && !folder.trashed)) {
      setCurrentFolderId(null);
    }
  }, [dashboard, currentFolderId]);

  useEffect(() => {
    const visible = new Set(files.map((file) => file.id));
    setSelectedFiles((current) => {
      const next = new Set([...current].filter((id) => visible.has(id)));
      return next.size === current.size ? current : next;
    });
  }, [files]);

  const filteredTransfers = useMemo(() => {
    if (!dashboard) return [];
    return dashboard.transfers.filter((job) => {
      if (transferFilter === "failed") return job.status === "failed";
      if (transferFilter === "completed") return job.status === "completed" || job.status === "duplicate";
      if (transferFilter === "pending") return isTransferPending(job);
      return true;
    });
  }, [dashboard, transferFilter]);

  const action = async (operation: () => Promise<unknown>) => {
    try {
      await operation();
      await refreshDashboard();
    } catch (error) {
      setAppNotice(readableError(error));
    }
  };

  const zipUploads = async (items: { path: string; name?: string }[], folderId?: string | null) => {
    setZipProgress({ processed: 0, total: 0, name: "Preparando ZIP…" });
    setAppNotice("Comprimiendo en paquetes ZIP de hasta 2 GB…");
    try {
      return await prepareZipUploads(items, folderId, (processed, total, name) => setZipProgress({ processed, total, name }));
    } finally { setZipProgress(null); }
  };
  const prepareSelectedUploads: typeof prepareUploadBatch = (paths, concurrency, onSettled, folderId) => zipBeforeUpload
    ? zipUploads(paths.map(path => ({ path })), folderId)
    : prepareUploadBatch(paths, concurrency, onSettled, folderId);

  const handleUpload = async () => {
    if (uploadBusy) return;
    if (!dashboard?.telegramConnected) {
      setConnectModal(true);
      return;
    }
    setUploadBusy(true);
    setAppNotice(null);
    try {
      const selected = await selectFilesForUpload();
      if (!selected.length) return;
      const targetFolderId = section === "files" ? currentFolderId : null;
      if (section !== "files") setCurrentFolderId(null);
      setSection("files");
      setMobileMenu(false);
      const results = await prepareSelectedUploads(
        selected,
        dashboard.settings.preparationConcurrency,
        (_result, completed, total) => setAppNotice(`Preparando archivos ${completed}/${total}…`),
        targetFolderId,
      );
      const queued = results.filter((result) => result.ok && !result.upload.duplicate).length;
      const duplicates = results.filter((result) => result.ok && result.upload.duplicate).length;
      const failed = results.filter((result): result is { ok: false; path: string; error: string } => !result.ok);
      if (failed.length > 0) {
        const skippedItems: SkippedUploadItem[] = failed.map((item) => {
          const fileName = item.path.split(/[\\/]/).pop() || item.path;
          return {
            fileName,
            path: item.path,
            reason: item.error,
          };
        });
        setSkippedFiles(skippedItems);
        setShowSkippedModal(true);
      }
      const parts = [
        queued ? `${queued} listo${queued === 1 ? "" : "s"} para subir` : null,
        duplicates ? `${duplicates} duplicado${duplicates === 1 ? "" : "s"}` : null,
        failed.length ? `${failed.length} omitido${failed.length === 1 ? "" : "s"} (límite Telegram)` : null,
      ].filter(Boolean);
      setAppNotice(parts.join(" · ") || "No se añadieron archivos");
      await refreshDashboard();
    } catch (error) {
      setAppNotice(`No se pudo preparar la selección: ${readableError(error)}`);
    } finally {
      setUploadBusy(false);
    }
  };

  const executeUploadFolderPlan = async (
    plan: DirectoryUploadPlan,
    targetFolderId: string | null,
  ): Promise<void> => {
    if (!plan.files.length && !plan.folders.length) {
      setAppNotice(`La carpeta "${plan.rootName}" está vacía.`);
      return;
    }
    if (zipBeforeUpload && plan.files.length > 0) {
      const results = await zipUploads(plan.files.map(file => ({ path: file.absolutePath, name: `${plan.rootName}/${file.relativePath}` })), targetFolderId);
      const queued = results.filter(result => result.ok && !result.upload.duplicate).length;
      const duplicates = results.filter(result => result.ok && result.upload.duplicate).length;
      const errors = results.filter(result => !result.ok);
      setAppNotice(`${queued} ZIP listos · ${duplicates} duplicados${errors.length ? ` · ${errors.map(result => result.ok ? "" : result.error).join("; ")}` : ""}`);
      return;
    }
    setAppNotice(`Creando estructura de carpeta "${plan.rootName}"…`);

    const knownFolders = [...(dashboard?.folders || [])];
    const folderMap = new Map<string, string>();

    const existingRoot = knownFolders.find(
      (f) => !f.trashed && f.parentId === targetFolderId && f.name.toLowerCase() === plan.rootName.toLowerCase(),
    );
    let rootId: string;
    if (existingRoot) {
      rootId = existingRoot.id;
    } else {
      rootId = await createFolder(plan.rootName, targetFolderId);
      knownFolders.push({
        id: rootId,
        name: plan.rootName,
        parentId: targetFolderId,
        trashed: false,
        createdAt: Date.now() / 1000,
        updatedAt: Date.now() / 1000,
        fileCount: 0,
        childCount: 0,
        sizeBytes: 0,
      });
    }
    folderMap.set("", rootId);

    for (const relFolder of plan.folders) {
      const segments = relFolder.split("/");
      const folderName = segments[segments.length - 1];
      const parentRel = segments.slice(0, -1).join("/");
      const parentFolderId = folderMap.get(parentRel) ?? rootId;

      const existingSub = knownFolders.find(
        (f) => !f.trashed && f.parentId === parentFolderId && f.name.toLowerCase() === folderName.toLowerCase(),
      );
      if (existingSub) {
        folderMap.set(relFolder, existingSub.id);
      } else {
        const newId = await createFolder(folderName, parentFolderId);
        knownFolders.push({
          id: newId,
          name: folderName,
          parentId: parentFolderId,
          trashed: false,
          createdAt: Date.now() / 1000,
          updatedAt: Date.now() / 1000,
          fileCount: 0,
          childCount: 0,
          sizeBytes: 0,
        });
        folderMap.set(relFolder, newId);
      }
    }

    const itemsToPrepare = plan.files.map((file) => {
      const parts = file.relativePath.split("/");
      const fileFolderRel = parts.slice(0, -1).join("/");
      const fileFolderId = folderMap.get(fileFolderRel) ?? rootId;
      return {
        path: file.absolutePath,
        folderId: fileFolderId,
      };
    });

    if (itemsToPrepare.length > 0) {
      const concurrency = dashboard?.settings.preparationConcurrency || 4;
      const results = await prepareUploadItemsBatch(
        itemsToPrepare,
        concurrency,
        (_result, completed, total) => setAppNotice(`Preparando "${plan.rootName}" (${completed}/${total})…`),
      );
      const queued = results.filter((result) => result.ok && !result.upload.duplicate).length;
      const duplicates = results.filter((result) => result.ok && result.upload.duplicate).length;
      const failed = results.filter((result): result is { ok: false; path: string; error: string } => !result.ok);
      if (failed.length > 0) {
        const skippedItems: SkippedUploadItem[] = failed.map((item) => {
          const scanned = plan.files.find((f) => f.absolutePath === item.path);
          const fileName = scanned ? scanned.relativePath : (item.path.split(/[\\/]/).pop() || item.path);
          return {
            fileName,
            path: item.path,
            reason: item.error,
            sizeBytes: scanned?.size,
          };
        });
        setSkippedFiles((prev) => [...prev, ...skippedItems]);
        setShowSkippedModal(true);
      }
      const parts = [
        queued ? `${queued} listo${queued === 1 ? "" : "s"} para subir` : null,
        duplicates ? `${duplicates} duplicado${duplicates === 1 ? "" : "s"}` : null,
        failed.length ? `${failed.length} omitido${failed.length === 1 ? "" : "s"} (límite Telegram)` : null,
      ].filter(Boolean);
      setAppNotice(`Carpeta "${plan.rootName}": ${parts.join(" · ") || "Estructura creada"}`);
    } else {
      setAppNotice(`Carpeta "${plan.rootName}" creada con ${plan.folders.length} subcarpeta${plan.folders.length === 1 ? "" : "s"}`);
    }
  };

  const handleUploadFolder = async () => {
    if (uploadBusy) return;
    if (!dashboard?.telegramConnected) {
      setConnectModal(true);
      return;
    }
    setUploadBusy(true);
    setAppNotice(null);
    try {
      const plan = await selectFolderForUpload();
      if (!plan) return;
      const targetFolderId = section === "files" ? currentFolderId : null;
      if (section !== "files") setCurrentFolderId(null);
      setSection("files");
      setMobileMenu(false);
      await executeUploadFolderPlan(plan, targetFolderId);
      await refreshDashboard();
    } catch (error) {
      setAppNotice(`No se pudo subir la carpeta: ${readableError(error)}`);
    } finally {
      setUploadBusy(false);
    }
  };

  const handleExternalDrop = async (paths: string[], targetFolderId: string | null) => {
    if (uploadBusy) return;
    if (!dashboard?.telegramConnected) {
      setAppNotice("Conecta Telegram para subir archivos a Nuvio.");
      setConnectModal(true);
      return;
    }
    setUploadBusy(true);
    setAppNotice("Analizando elementos arrastrados…");
    try {
      const inspected = await inspectDroppedPaths(paths);
      const directories = inspected.filter((item) => item.isDir);
      const files = inspected.filter((item) => !item.isDir);

      if (section !== "files") {
        setCurrentFolderId(targetFolderId);
        setSection("files");
      }

      for (const dir of directories) {
        setAppNotice(`Escaneando carpeta "${dir.name}"…`);
        const plan = await scanDirectoryForUpload(dir.path);
        await executeUploadFolderPlan(plan, targetFolderId);
      }

      if (files.length > 0) {
        const filePaths = files.map((f) => f.path);
        setAppNotice(`Preparando ${files.length} archivo${files.length === 1 ? "" : "s"}…`);
        const concurrency = dashboard?.settings.preparationConcurrency || 4;
        const results = await prepareSelectedUploads(
          filePaths,
          concurrency,
          (_result, completed, total) => setAppNotice(`Preparando archivos (${completed}/${total})…`),
          targetFolderId,
        );
        const queued = results.filter((result) => result.ok && !result.upload.duplicate).length;
        const duplicates = results.filter((result) => result.ok && result.upload.duplicate).length;
        const failed = results.filter((result): result is { ok: false; path: string; error: string } => !result.ok);
        if (failed.length > 0) {
          const skippedItems: SkippedUploadItem[] = failed.map((item) => ({
            fileName: item.path.split(/[\\/]/).pop() || item.path,
            path: item.path,
            reason: item.error,
          }));
          setSkippedFiles((prev) => [...prev, ...skippedItems]);
          setShowSkippedModal(true);
        }
        const parts = [
          queued ? `${queued} listo${queued === 1 ? "" : "s"} para subir` : null,
          duplicates ? `${duplicates} duplicado${duplicates === 1 ? "" : "s"}` : null,
          failed.length ? `${failed.length} omitido${failed.length === 1 ? "" : "s"} (límite Telegram)` : null,
        ].filter(Boolean);
        if (directories.length === 0) {
          setAppNotice(`Archivos: ${parts.join(" · ") || "Listo"}`);
        }
      }

      await refreshDashboard();
    } catch (error) {
      setAppNotice(`Error al procesar arrastre: ${readableError(error)}`);
    } finally {
      setUploadBusy(false);
    }
  };

  const handleSync = async () => {
    if (syncBusy) return;
    setSyncBusy(true);
    await action(async () => {
      const count = await syncFiles();
      setAppNotice(`${count} archivos de Nuvio encontrados en Mensajes guardados.`);
    });
    setSyncBusy(false);
  };

  const handleBulkDownload = async (ids = [...selectedFiles]) => {
    if (!ids.length || downloadBusyRef.current) return;
    if (!dashboard?.telegramConnected) { setMobileMenu(false); setConnectModal(true); return; }
    if (ids.length > 500) { setAppNotice("Selecciona como máximo 500 archivos por lote."); return; }
    downloadBusyRef.current = true;
    try {
      const directory = await selectDownloadDirectory();
      if (!directory) return;
      const results = await queueDownloads(ids, directory, dashboard?.settings.conflictPolicy);
      const queued = results.filter((item) => item.status === "queued").length;
      const skipped = results.filter((item) => item.status === "skipped").length;
      const errors = results.filter((item) => item.status === "error").length;
      setAppNotice([
        queued ? `${queued} descarga${queued === 1 ? "" : "s"} en cola` : null,
        skipped ? `${skipped} omitido${skipped === 1 ? "" : "s"}` : null,
        errors ? `${errors} error${errors === 1 ? "" : "es"}` : null,
      ].filter(Boolean).join(" · ") || "Sin cambios");
      setSelectedFiles(new Set());
      await refreshDashboard();
    } catch (error) {
      setAppNotice(readableError(error));
    } finally {
      downloadBusyRef.current = false;
    }
  };

  const closeMedia = () => {
    mediaRequestRef.current++;
    setMediaFile(null);
    setMediaSrc(null);
    setPreviewText(null);
    setMediaBusy(false);
    setMediaError(null);
  };

  const handleMedia = async (file: CloudFile) => {
    if (!canPreview(file.kind)) return;
    if (!dashboard?.telegramConnected) {
      setConnectModal(true);
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
        if (file.sizeBytes > 2 * 1024 * 1024) {
          setMediaError("La vista previa de texto está limitada a 2 MB. Descarga el archivo para verlo completo.");
        } else {
          const response = await fetch(source);
          if (!response.ok) throw new Error("No se pudo leer la vista previa de texto");
          const text = await response.text();
          if (request !== mediaRequestRef.current) return;
          setPreviewText(text);
        }
      }
      setAppNotice(ready.fromCache ? "Vista previa abierta desde la caché privada." : "Archivo preparado para vista previa en la caché privada de Nuvio.");
      await refreshDashboard();
    } catch (error) {
      if (request !== mediaRequestRef.current) return;
      setAppNotice(`No se pudo preparar la vista previa: ${readableError(error)}`);
      setMediaFile(null);
    } finally {
      if (request === mediaRequestRef.current) setMediaBusy(false);
    }
  };

  const handleClearCache = async () => {
    try {
      const released = await clearMediaCache();
      clearThumbnailCache();
      setAppNotice(`Caché multimedia limpiada · ${formatBytes(released)} liberados.`);
      closeMedia();
      await refreshDashboard();
    } catch (error) {
      setAppNotice(readableError(error));
    }
  };

  const handleExportDiagnostics = async () => {
    try {
      if (await exportDiagnostics()) setAppNotice("Diagnóstico de Nuvio exportado sin credenciales ni códigos de acceso.");
    } catch (error) {
      setAppNotice(readableError(error));
    }
  };

  const handleClearHistory = async () => {
    if (!dashboard?.transferHistory.length) return;
    if (!window.confirm("¿Limpiar el historial de transferencias? Los archivos almacenados no se eliminarán.")) return;
    try {
      const removed = await clearTransferHistory();
      setAppNotice(`${removed} transferencia${removed === 1 ? "" : "s"} eliminada${removed === 1 ? "" : "s"} del historial.`);
      await refreshDashboard();
    } catch (error) {
      setAppNotice(readableError(error));
    }
  };

  const handleSaveFolder = async (name: string) => {
    if (!folderEditor) return;
    try {
      if (folderEditor.mode === "create") {
        await createFolder(name, currentFolderId);
        setAppNotice(`Carpeta “${name.trim()}” creada y sincronizada con Telegram.`);
      } else if (folderEditor.folder) {
        await renameFolder(folderEditor.folder.id, name);
        setAppNotice(`Carpeta renombrada a “${name.trim()}”.`);
      }
      setFolderEditor(null);
      await refreshDashboard();
    } catch (error) {
      throw new Error(readableError(error));
    }
  };

  const handleMoveConfirm = async (targetFolderId: string | null) => {
    if (!moveDialog) return;
    try {
      if (moveDialog.kind === "files") {
        const changed = await moveFilesToFolder(moveDialog.ids, targetFolderId);
        setSelectedFiles(new Set());
        setAppNotice(`${changed} archivo${changed === 1 ? "" : "s"} movido${changed === 1 ? "" : "s"}.`);
      } else {
        await moveFolder(moveDialog.folder.id, targetFolderId);
        setAppNotice(`Carpeta “${moveDialog.folder.name}” movida.`);
      }
      setMoveDialog(null);
      await refreshDashboard();
    } catch (error) {
      throw new Error(readableError(error));
    }
  };

  const dragIdsFor = (file: CloudFile) => selectedFiles.has(file.id) && selectedFiles.size > 0
    ? [...selectedFiles]
    : [file.id];

  const clearFileDrag = () => {
    draggingFileIdsRef.current = [];
    const touch = touchDragRef.current;
    if (touch?.active) suppressClickUntilRef.current = Date.now() + 400;
    if (touch?.element.hasPointerCapture(touch.pointerId)) touch.element.releasePointerCapture(touch.pointerId);
    if (touch?.timer != null) window.clearTimeout(touch.timer);
    touchDragRef.current = null;
    setDraggingFileIds([]);
    setDragTarget(null);
    setTouchDragPosition(null);
  };

  const performFileDrop = async (ids: string[], targetFolderId: string | null) => {
    if (!ids.length) return;
    const currentDashboard = dashboard;
    if (!currentDashboard) return;
    if (!currentDashboard.telegramConnected) {
      clearFileDrag();
      setConnectModal(true);
      return;
    }
    const alreadyThere = ids.every((id) => {
      const file = currentDashboard.files.find((item) => item.id === id);
      return (file?.folderId ?? null) === targetFolderId;
    });
    if (alreadyThere) {
      clearFileDrag();
      setAppNotice("Los archivos ya están en esa ubicación.");
      return;
    }
    const destination = targetFolderId
      ? currentDashboard.folders.find((folder) => folder.id === targetFolderId)?.name ?? "la carpeta"
      : "Mi unidad";
    setAppNotice(`Moviendo ${ids.length} archivo${ids.length === 1 ? "" : "s"} a ${destination}…`);
    try {
      const changed = await moveFilesToFolder(ids, targetFolderId);
      setSelectedFiles(new Set());
      setAppNotice(`${changed} archivo${changed === 1 ? "" : "s"} movido${changed === 1 ? "" : "s"} a ${destination}.`);
      await refreshDashboard();
    } catch (error) {
      setAppNotice(readableError(error));
    } finally {
      clearFileDrag();
    }
  };

  const desktopDragStart = (event: React.DragEvent<HTMLElement>, file: CloudFile) => {
    if (file.trashed) {
      event.preventDefault();
      return;
    }
    const ids = dragIdsFor(file);
    suppressClickUntilRef.current = Date.now() + 400;
    event.dataTransfer.effectAllowed = "copyMove";
    event.dataTransfer.setData("application/x-nuvio-files", JSON.stringify(ids));
    event.dataTransfer.setData("text/plain", ids.join(","));
    draggingFileIdsRef.current = ids;
    setDraggingFileIds(ids);
    setDragTarget(null);
  };

  const desktopDrop = (event: React.DragEvent<HTMLElement>, targetFolderId: string | null) => {
    event.preventDefault();
    event.stopPropagation();
    let ids = draggingFileIdsRef.current.length ? draggingFileIdsRef.current : draggingFileIds;
    try {
      const raw = event.dataTransfer.getData("application/x-nuvio-files");
      if (raw) {
        const parsed = JSON.parse(raw);
        if (Array.isArray(parsed) && parsed.every((value) => typeof value === "string") && parsed.length > 0) {
          ids = parsed;
        }
      }
    } catch { /* Use in-memory drag state. */ }
    void performFileDrop(ids, targetFolderId);
  };

  const dropTargetKeyAt = (x: number, y: number): string | null => {
    if (!Number.isFinite(x) || !Number.isFinite(y)) return null;
    return document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-nuvio-drop-folder]")?.dataset.nuvioDropFolder ?? null;
  };

  const activateTouchDrag = (event: React.PointerEvent<HTMLElement>) => {
    const touch = touchDragRef.current;
    if (!touch || touch.pointerId !== event.pointerId || touch.active) return;
    touch.active = true;
    touch.timer = null;
    suppressClickUntilRef.current = Date.now() + 500;
    try { event.currentTarget.setPointerCapture(event.pointerId); } catch { /* Some webviews may reject capture. */ }
    touch.lastX = event.clientX;
    touch.lastY = event.clientY;
    setDraggingFileIds(touch.ids);
    draggingFileIdsRef.current = touch.ids;
    setTouchDragPosition({ x: event.clientX, y: event.clientY });
    const target = dropTargetKeyAt(event.clientX, event.clientY);
    touch.lastTarget = target;
    setDragTarget(target);
    try { navigator.vibrate?.(25); } catch { /* Vibration is optional. */ }
  };

  const touchDragStart = (event: React.PointerEvent<HTMLElement>, file: CloudFile) => {
    if (file.trashed) return;
    const interactive = (event.target as Element).closest("button,input,label,select,a");
    if (interactive || !event.isPrimary) return;
    if (event.pointerType === "mouse" && event.button !== 0) return;
    clearFileDrag();
    const ids = dragIdsFor(file);
    const isMouse = event.pointerType === "mouse";
    const state = {
      pointerId: event.pointerId,
      ids,
      startX: event.clientX,
      startY: event.clientY,
      lastX: event.clientX,
      lastY: event.clientY,
      lastTarget: null as string | null,
      active: false,
      timer: null as number | null,
      element: event.currentTarget,
    };
    touchDragRef.current = state;
    if (isMouse) {
      state.timer = null;
    } else if (!selectedFiles.has(file.id)) {
      state.timer = window.setTimeout(() => {
        const current = touchDragRef.current;
        if (!current || current.pointerId !== event.pointerId || current.active) return;
        current.active = true;
        current.timer = null;
        suppressClickUntilRef.current = Date.now() + 500;
        try { current.element.setPointerCapture(current.pointerId); } catch { /* Pointer capture is optional. */ }
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

  const touchDragMove = (event: React.PointerEvent<HTMLElement>) => {
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
    const target = dropTargetKeyAt(event.clientX, event.clientY);
    touch.lastTarget = target;
    setDragTarget(target);
  };

  const touchDragEnd = (event: React.PointerEvent<HTMLElement>) => {
    const touch = touchDragRef.current;
    if (!touch || touch.pointerId !== event.pointerId) return;
    if (touch.timer != null) window.clearTimeout(touch.timer);
    try { event.currentTarget.releasePointerCapture(event.pointerId); } catch { /* Optional */ }
    if (!touch.active) {
      touchDragRef.current = null;
      return;
    }
    event.preventDefault();
    const key = dropTargetKeyAt(event.clientX, event.clientY);
    const ids = touch.ids;
    clearFileDrag();
    if (key == null) {
      setAppNotice("Movimiento cancelado.");
      return;
    }
    try { navigator.vibrate?.(30); } catch { /* Optional. */ }
    void performFileDrop(ids, key === "__root__" ? null : key);
  };

  const handleDeleteFolder = async (folder: CloudFolder) => {
    if (!window.confirm(`¿Eliminar la carpeta “${folder.name}”?\n\nPor seguridad, Nuvio solo elimina carpetas vacías.`)) return;
    try {
      await deleteFolder(folder.id);
      setAppNotice(`Carpeta “${folder.name}” eliminada.`);
      await refreshDashboard();
    } catch (error) {
      setAppNotice(readableError(error));
    }
  };

  const scrollFileTypes = (direction: -1 | 1) => {
    filterTabsRef.current?.scrollBy({ left: direction * 280, behavior: "smooth" });
  };

  const handleBulkTrash = async (ids: string[], trashed: boolean) => {
    if (!ids.length) return;
    try {
      const changed = await setTrashedMany(ids, trashed);
      setSelectedFiles(new Set());
      setAppNotice(
        trashed
          ? `${changed} archivo${changed === 1 ? "" : "s"} movido${changed === 1 ? "" : "s"} a la Papelera.`
          : `${changed} archivo${changed === 1 ? "" : "s"} restaurado${changed === 1 ? "" : "s"}.`,
      );
      await refreshDashboard();
    } catch (error) {
      setAppNotice(readableError(error));
    }
  };

  const handlePermanentDelete = async (ids: string[]) => {
    if (!ids.length) return;
    if (!dashboard?.telegramConnected) {
      setConnectModal(true);
      return;
    }
    const count = ids.length;
    const confirmed = window.confirm(
      `¿Eliminar definitivamente ${count} archivo${count === 1 ? "" : "s"}?\n\nTambién se eliminará${count === 1 ? "" : "n"} de Mensajes guardados de Telegram. Esta acción no se puede deshacer.`,
    );
    if (!confirmed) return;
    try {
      const removed = await deleteFilesPermanently(ids);
      if (mediaFile && ids.includes(mediaFile.id)) closeMedia();
      setSelectedFiles(new Set());
      setAppNotice(`${removed} archivo${removed === 1 ? "" : "s"} eliminado${removed === 1 ? "" : "s"} definitivamente.`);
      await refreshDashboard();
    } catch (error) {
      setAppNotice(readableError(error));
    }
  };

  const handleEmptyTrash = async () => {
    const trashCount = dashboard?.files.filter((file) => file.trashed).length ?? 0;
    if (!trashCount) return;
    if (!dashboard?.telegramConnected) {
      setConnectModal(true);
      return;
    }
    const confirmed = window.confirm(
      `¿Vaciar la Papelera?\n\nSe eliminarán definitivamente ${trashCount} archivo${trashCount === 1 ? "" : "s"} de Nuvio y de Mensajes guardados de Telegram. Esta acción no se puede deshacer.`,
    );
    if (!confirmed) return;
    try {
      const removed = await emptyTrash();
      closeMedia();
      setSelectedFiles(new Set());
      setAppNotice(`Papelera vaciada · ${removed} archivo${removed === 1 ? "" : "s"} eliminado${removed === 1 ? "" : "s"}.`);
      await refreshDashboard();
    } catch (error) {
      setAppNotice(readableError(error));
    }
  };

  const toggleSelection = (id: string) => {
    if (Date.now() < suppressClickUntilRef.current) return;
    setSelectedFiles((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  };

  const allVisibleSelected = files.length > 0 && files.every((file) => selectedFiles.has(file.id));
  const toggleAllVisible = () => {
    setSelectedFiles((current) => {
      const next = new Set(current);
      if (allVisibleSelected) files.forEach((file) => next.delete(file.id));
      else files.forEach((file) => next.add(file.id));
      return next;
    });
  };

  if (!dashboard) {
    return (
      <main className="boot-screen">
        <div className="brand-mark large"><Cloud size={28} /></div>
        <div className="boot-copy">{loadError ? "No se pudo abrir Nuvio" : "Preparando tu nube…"}</div>
        {loadError && <p className="modal-error" role="alert">{loadError}</p>}
        {loadError && <button className="primary-button" onClick={() => void refreshDashboard()}>Reintentar</button>}
      </main>
    );
  }

  const activeSection = section === "files" && currentFolder
    ? currentFolder.name
    : navItems.find((item) => item.key === section)?.label ?? "Inicio";
  const queue = dashboard.queueSummary;
  const queueProgress = queue.totalBytes > 0 ? Math.round((queue.processedBytes / queue.totalBytes) * 100) : 0;
  const queueHasActive = queue.active > 0;
  const queueHasPaused = dashboard.transfers.some((job) => job.status === "paused");

  return (
    <div className="app-shell">
      <aside className={`sidebar ${mobileMenu ? "mobile-open" : ""}`}>
        <div className="brand-row">
          <div className="brand-mark"><Cloud size={19} strokeWidth={2.4} /></div>
          <div><div className="brand-name">Nuvio</div><div className="brand-caption">private cloud</div></div>
          <button className="icon-button sidebar-close" onClick={() => setMobileMenu(false)} aria-label="Cerrar menú"><X size={19} /></button>
        </div>
        <button className="upload-primary" onClick={() => void handleUpload()} disabled={uploadBusy}>
          <Plus size={18} /><span>{uploadBusy ? "Preparando…" : "Subir archivos"}</span>
        </button>
        <button className="upload-folder-sidebar" onClick={() => void handleUploadFolder()} disabled={uploadBusy}>
          <FolderUp size={17} /><span>Subir carpeta</span>
        </button>
        <nav className="main-nav" aria-label="Principal">
          {navItems.map((item) => {
            const Icon = item.icon;
            return <button key={item.key} className={`nav-item ${section === item.key ? "active" : ""}`} onClick={() => { if (item.key === "files") setCurrentFolderId(null); setSection(item.key); setMobileMenu(false); }}>
              <Icon size={18} /><span>{item.label}</span>
              {item.key === "favorites" && dashboard.favoriteCount > 0 && <span className="nav-count">{dashboard.favoriteCount}</span>}
              {item.key === "history" && dashboard.transferHistory.length > 0 && <span className="nav-count">{dashboard.transferHistory.length}</span>}
            </button>;
          })}
        </nav>
        <div className="sidebar-group">
          <div className="sidebar-label">Etiquetas</div>
          {tagOptions.map((tag, index) => <button key={tag} className={`tag-nav ${selectedTag === tag ? "active" : ""}`} onClick={() => setSelectedTag(selectedTag === tag ? null : tag)}>
            <span className={`tag-dot tag-${index + 1}`} /><span>{tag}</span>
          </button>)}
        </div>
        <div className="sidebar-spacer" />
        <div className="provider-card">
          <div className="provider-card-top"><span className={`status-dot ${dashboard.telegramConnected ? "online" : ""}`} /><span>Telegram</span><span className="provider-state">{dashboard.telegramConnected ? "Activo" : "Sin conectar"}</span></div>
          <div className="provider-description">{dashboard.providerStatus}</div>
          <div className="provider-file-count">{dashboard.fileCount} archivo{dashboard.fileCount === 1 ? "" : "s"} sincronizado{dashboard.fileCount === 1 ? "" : "s"}</div>
          <button className="provider-link" onClick={() => setConnectModal(true)}>{dashboard.telegramConnected ? "Ver cuenta" : "Configurar conexión"}</button>
        </div>
        <button className="nav-item settings-item" onClick={() => setConnectModal(true)}><Settings size={18} /><span>Ajustes</span></button>
      </aside>

      {mobileMenu && <button className="sidebar-scrim" onClick={() => setMobileMenu(false)} aria-label="Cerrar menú" />}

      <main className="main-content">
        <header className="topbar">
          <div className="mobile-brand"><button className="icon-button" onClick={() => setMobileMenu(true)} aria-label="Abrir menú"><Menu size={21} /></button><div className="brand-mark compact"><Cloud size={17} /></div></div>
          <div className="search-box">
            <Search size={18} />
            <input
              ref={searchInputRef}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Buscar archivos, carpetas o etiquetas"
              aria-label="Buscar"
            />
            {query ? (
              <button className="clear-search-btn" type="button" onClick={() => setQuery("")} aria-label="Limpiar búsqueda" title="Limpiar búsqueda">
                <X size={15} />
              </button>
            ) : (
              <kbd>Ctrl K</kbd>
            )}
          </div>
          <div className="topbar-actions">
            <button className="icon-button" onClick={() => setDarkMode((value) => !value)} aria-label="Cambiar tema">{darkMode ? <Sun size={18} /> : <Moon size={18} />}</button>
            <button className="profile-button" onClick={() => setConnectModal(true)} title={dashboard.telegramConnected ? "Telegram conectado" : "Conectar Telegram"}>
              <span className={`avatar ${dashboard.telegramConnected ? "is-connected" : ""}`}>{dashboard.telegramAccountLabel?.slice(0, 2).toUpperCase() || "NU"}</span>
              <span className="profile-copy"><strong>{dashboard.telegramAccountLabel || "Mi cuenta"}</strong><small>{dashboard.telegramConnected ? "Telegram conectado" : "Conectar Telegram"}</small></span>
              <ChevronDown size={15} />
            </button>
          </div>
        </header>

        <div className="content-scroll">
          <section className="page-heading">
            <div><div className="eyebrow">Tu espacio</div><h1>{activeSection}</h1><p>Archivos organizados, transferencias transparentes y control local.</p></div>
            <div className="heading-actions">
              {section === "files" && <button className="secondary-button folder-create-button" disabled={!dashboard.telegramConnected} onClick={() => setFolderEditor({ mode: "create" })}><FolderPlus size={17} /> Nueva carpeta</button>}
              <button className="secondary-button folder-upload-button" disabled={uploadBusy || !dashboard.telegramConnected} onClick={() => void handleUploadFolder()}><FolderUp size={17} /> Subir carpeta</button>
              <button className={`secondary-button sync-action-button ${syncBusy ? "is-syncing" : ""}`} disabled={syncBusy || !dashboard.telegramConnected} onClick={() => void handleSync()} title={syncBusy ? "Sincronizando con Telegram…" : "Sincronizar con Telegram"} aria-label={syncBusy ? "Sincronizando…" : "Sincronizar"}><RefreshCw size={17} className={syncBusy ? "spin-icon" : ""} /><span className="sync-button-label">{syncBusy ? "Sincronizando…" : "Sincronizar"}</span></button>
              <button className="primary-button" onClick={() => void handleUpload()} disabled={uploadBusy}><Upload size={17} /> {uploadBusy ? "Preparando…" : "Subir"}</button>
            </div>
          </section>

          <div className="zip-upload-options">
            <label><input type="checkbox" checked={zipBeforeUpload} disabled={uploadBusy} onChange={event => setZipBeforeUpload(event.target.checked)} /> Comprimir antes de subir (ZIP, máximo 2 GB por paquete)</label>
            {zipBeforeUpload && <small>Conserva los originales y la ruta de cada archivo dentro del ZIP. Si la selección es grande, crea varios ZIP independientes. Un archivo que no cabe comprimido en 2 GB se informa sin subirlo. Fotos y vídeos pueden ahorrar poco espacio.</small>}
          </div>
          {zipProgress && <section className="sync-progress-panel" aria-label="Progreso de compresión">
            <div><strong>{zipProgress.processed === zipProgress.total && zipProgress.total > 0 ? "Preparando ZIP para subir…" : "Comprimiendo…"}</strong><span>{zipProgress.total > 0 ? `${Math.min(100, Math.floor(zipProgress.processed * 100 / zipProgress.total))}%` : "Calculando…"}</span></div>
            <progress aria-label="Compresión ZIP" max={zipProgress.total || 1} value={zipProgress.total ? zipProgress.processed : undefined} />
            <small>{zipProgress.name}</small>
          </section>}

          {(dashboard.syncProgress?.phase || syncBusy) && <section className="sync-progress-panel" aria-label="Progreso de sincronización">
            <div><strong>{dashboard.syncProgress?.active ? (dashboard.syncProgress.phase === "applying" ? "Actualizando catálogo…" : "Sincronizando…") : syncBusy ? "Iniciando sincronización…" : dashboard.syncProgress?.error ? "Sincronización interrumpida" : "Sincronización completada"}</strong>
              <span>{dashboard.syncProgress?.active ? (dashboard.syncProgress.percent == null ? "Calculando…" : `${dashboard.syncProgress.percent}% aprox.`) : syncBusy ? "Calculando…" : dashboard.syncProgress?.error ? "Pendiente de reintentar" : "100%"}</span></div>
            <progress aria-label="Sincronización" max={100} value={syncBusy && !dashboard.syncProgress?.active ? undefined : dashboard.syncProgress?.percent ?? undefined} />
            <small>{dashboard.syncProgress?.error || (dashboard.syncProgress?.active ? `${dashboard.syncProgress.scanned} mensajes revisados${dashboard.syncProgress.etaSeconds != null ? ` · Quedan aproximadamente ${dashboard.syncProgress.etaSeconds < 60 ? `${dashboard.syncProgress.etaSeconds} s` : `${Math.ceil(dashboard.syncProgress.etaSeconds / 60)} min`}` : ""}` : syncBusy ? "Consultando Telegram…" : "Catálogo actualizado")}</small>
          </section>}

          {section === "files" && <nav className="folder-breadcrumbs" aria-label="Ruta de carpetas">
            <button
              data-nuvio-drop-folder="__root__"
              className={`${!currentFolderId ? "active" : ""} ${dragTarget === "__root__" ? "drag-over" : ""}`}
              onClick={() => setCurrentFolderId(null)}
              onDragEnter={(event) => { event.preventDefault(); event.stopPropagation(); event.dataTransfer.dropEffect = "move"; if (dragTarget !== "__root__") setDragTarget("__root__"); }}
              onDragOver={(event) => { event.preventDefault(); event.stopPropagation(); event.dataTransfer.dropEffect = "move"; if (dragTarget !== "__root__") setDragTarget("__root__"); }}
              onDragLeave={(event) => { if (event.currentTarget.contains(event.relatedTarget as Node)) return; if (dragTarget === "__root__") setDragTarget(null); }}
              onDrop={(event) => { event.preventDefault(); event.stopPropagation(); desktopDrop(event, null); }}
            ><HardDrive size={14} /> Mi unidad</button>
            {folderBreadcrumbs.map((folder) => <span key={folder.id} className="breadcrumb-part"><ChevronRight size={13} /><button
              data-nuvio-drop-folder={folder.id}
              className={`${folder.id === currentFolderId ? "active" : ""} ${dragTarget === folder.id ? "drag-over" : ""}`}
              onClick={() => setCurrentFolderId(folder.id)}
              onDragEnter={(event) => { event.preventDefault(); event.stopPropagation(); event.dataTransfer.dropEffect = "move"; if (dragTarget !== folder.id) setDragTarget(folder.id); }}
              onDragOver={(event) => { event.preventDefault(); event.stopPropagation(); event.dataTransfer.dropEffect = "move"; if (dragTarget !== folder.id) setDragTarget(folder.id); }}
              onDragLeave={(event) => { if (event.currentTarget.contains(event.relatedTarget as Node)) return; if (dragTarget === folder.id) setDragTarget(null); }}
              onDrop={(event) => { event.preventDefault(); event.stopPropagation(); desktopDrop(event, folder.id); }}
            >{folder.name}</button></span>)}
          </nav>}

          {skippedFiles.length > 0 && (
            <div className="skipped-files-banner" role="alert">
              <div className="skipped-banner-left">
                <AlertTriangle size={18} />
                <span>
                  <strong>{skippedFiles.length} archivo{skippedFiles.length === 1 ? "" : "s"} omitido{skippedFiles.length === 1 ? "" : "s"}</strong> por límite de Telegram ({dashboard.isPremium ? "4 GB" : "2 GB"}). El resto continúa procesándose.
                </span>
              </div>
              <div className="skipped-banner-actions">
                <button
                  type="button"
                  className="skipped-banner-btn"
                  onClick={() => setShowSkippedModal(true)}
                >
                  Ver omitidos
                </button>
                <button
                  type="button"
                  className="ghost-icon"
                  onClick={() => setSkippedFiles([])}
                  title="Descartar aviso"
                  aria-label="Descartar aviso"
                >
                  <X size={15} />
                </button>
              </div>
            </div>
          )}

          {appNotice && (
            <div className="app-notice" role="status">
              <span>{appNotice}</span>
              {skippedFiles.length > 0 && (
                <button
                  type="button"
                  className="app-notice-action"
                  onClick={() => setShowSkippedModal(true)}
                >
                  <AlertTriangle size={13} />
                  Ver omitidos ({skippedFiles.length})
                </button>
              )}
              <button className="ghost-icon" onClick={() => setAppNotice(null)} aria-label="Cerrar aviso">
                <X size={15} />
              </button>
            </div>
          )}
          {loadError && <div className="modal-error" role="alert">{loadError}</div>}

          {section === "home" && <section className="hero-grid">
            <article className="hero-card primary-hero">
              <div className="hero-glow" /><div className="hero-icon"><Zap size={21} /></div>
              <div className="hero-copy"><span className="hero-kicker">Nuvio Cloud</span><h2>Tu nube, sin ruido.</h2><p>Mensajes guardados de Telegram como proveedor remoto y un índice local rápido.</p></div>
              <button className="hero-button" onClick={() => setConnectModal(true)}>{dashboard.telegramConnected ? "Ver conexión" : "Conectar Telegram"}<span>→</span></button>
            </article>
            <article className="metric-card"><div className="metric-icon"><HardDrive size={20} /></div><span className="metric-label">Indexado</span><strong>{formatBytes(dashboard.totalBytes)}</strong><small>{dashboard.fileCount} archivos</small></article>
            <article className="metric-card"><div className="metric-icon"><Sparkles size={20} /></div><span className="metric-label">Caché multimedia</span><strong>{formatBytes(queue.cacheBytes)}</strong><small>de {formatBytes(queue.cacheLimitBytes)}</small><div className="metric-track"><span style={{ width: `${Math.min(100, queue.cacheLimitBytes ? queue.cacheBytes / queue.cacheLimitBytes * 100 : 0)}%` }} /></div>{queue.cacheBytes > 0 && <button className="metric-link" onClick={() => void handleClearCache()}>Limpiar caché</button>}</article>
          </section>}

          {queue.total > 0 && <section className="queue-panel" aria-label="Cola de transferencias">
            <div className="queue-summary-head">
              <div><span className="eyebrow">Transferencias</span><h2>Cola</h2></div>
              <div className="queue-actions">
                <button className="secondary-button" onClick={() => setSection("history")}><Clock3 size={15} /> Historial</button>
                <button className="secondary-button" onClick={() => void handleExportDiagnostics()}><FileText size={15} /> Diagnóstico</button>
                {queueHasActive && <button className="secondary-button" onClick={() => void action(pauseQueue)}><Pause size={15} /> Pausar cola</button>}
                {queueHasPaused && <button className="secondary-button" onClick={() => void action(resumeQueue)}><Play size={15} /> Continuar</button>}
              </div>
            </div>
            <div className="queue-overview">
              <div className="stat-card completed">
                <div className="stat-card-header">
                  <CheckCircle2 size={16} className="stat-icon-success" />
                  <span>Completados</span>
                </div>
                <strong>{queue.completed}</strong>
                <small>archivos finalizados</small>
              </div>
              <div className={`stat-card ${queue.failed + queue.pending + skippedFiles.length > 0 ? "uncompleted" : ""}`}>
                <div className="stat-card-header">
                  <AlertCircle size={16} className="stat-icon-warning" />
                  <span>No completados</span>
                </div>
                <strong>{queue.failed + queue.pending + skippedFiles.length}</strong>
                <small>
                  {queue.failed > 0 && `${queue.failed} error`}
                  {queue.failed > 0 && queue.pending > 0 && " · "}
                  {queue.pending > 0 && `${queue.pending} en espera`}
                  {(queue.failed > 0 || queue.pending > 0) && skippedFiles.length > 0 && " · "}
                  {skippedFiles.length > 0 && `${skippedFiles.length} omitidos`}
                  {queue.failed === 0 && queue.pending === 0 && skippedFiles.length === 0 && "cola al día"}
                </small>
              </div>
              <div className="stat-card">
                <div className="stat-card-header">
                  <Zap size={16} />
                  <span>Velocidad</span>
                </div>
                <strong>{formatSpeed(queue.speedBps)}</strong>
                <small>{queue.active > 0 ? `${queue.active} en simultáneo` : "inactivo"}</small>
              </div>
              <div className="stat-card">
                <div className="stat-card-header">
                  <Clock3 size={16} />
                  <span>Tiempo restante</span>
                </div>
                <strong>{formatEta(queue.etaSeconds, queue.active > 0)}</strong>
                <small>{formatBytes(queue.processedBytes)} de {formatBytes(queue.totalBytes)}</small>
              </div>
            </div>
            <div className="queue-global-progress"><div><span>{formatBytes(queue.processedBytes)} / {formatBytes(queue.totalBytes)}</span><strong>{queueProgress}%</strong></div><progress max={100} value={queueProgress} /></div>
            <div className="transfer-filter-row">
              {(["all", "pending", "failed"] as TransferFilter[]).map((filter) => <button key={filter} className={transferFilter === filter ? "active" : ""} onClick={() => setTransferFilter(filter)}>{({ all: "Todas", pending: "Pendientes", failed: "Fallidas", completed: "Completadas" } as Record<TransferFilter, string>)[filter]}</button>)}
            </div>
            <div className="transfer-history">
              {filteredTransfers.map((job) => <TransferRow key={job.id} job={job} connected={dashboard.telegramConnected} onAction={(operation) => void action(operation)} />)}
            </div>
          </section>}

          {section === "history" && <section className="history-section">
            <div className="section-toolbar-top">
              <div><h2>Historial de transferencias</h2><span>{dashboard.transferHistory.length} registros</span></div>
              {dashboard.transferHistory.length > 0 && <button className="secondary-button" onClick={() => void handleClearHistory()}><Trash2 size={15} /> Limpiar historial</button>}
            </div>
            <p className="history-description">La cola activa se limpia automáticamente al terminar. Aquí puedes consultar subidas, descargas, duplicados y transferencias canceladas anteriores.</p>
            {dashboard.transferHistory.length === 0 ? <div className="empty-state"><div className="empty-icon"><Clock3 size={25} /></div><h3>Aún no hay historial</h3><p>Las transferencias terminadas aparecerán aquí sin ocupar espacio en la cola activa.</p></div> : <div className="history-list">{dashboard.transferHistory.map((job) => <TransferHistoryRow key={job.id} job={job} />)}</div>}
          </section>}

          {section !== "history" && <section className="files-section">
            <div className="section-toolbar-top">
              <div><h2>{section === "home" ? "Archivos recientes" : activeSection}</h2><span>{files.length + (section === "files" ? visibleFolders.length : 0)} elementos</span></div>
              {section === "home" && <button className="text-button" onClick={() => { setCurrentFolderId(null); setSection("files"); }}>Ver todos →</button>}
              {section === "trash" && files.length > 0 && <button className="secondary-button danger-button" onClick={() => void handleEmptyTrash()}><Trash2 size={15} /> Vaciar papelera</button>}
            </div>

            <div className="selection-toolbar">
              <label className="select-all-control"><input type="checkbox" checked={allVisibleSelected} onChange={toggleAllVisible} /> <span>Seleccionar visibles</span></label>
              {selectedFiles.size > 0 && <>
                <span className="selection-count">{selectedFiles.size} seleccionado{selectedFiles.size === 1 ? "" : "s"}</span>
                {section === "trash" ? <>
                  <button className="secondary-button" onClick={() => void handleBulkTrash([...selectedFiles], false)}><RotateCcw size={15} /> Restaurar</button>
                  <button className="secondary-button danger-button" onClick={() => void handlePermanentDelete([...selectedFiles])}><Trash2 size={15} /> Eliminar definitivamente</button>
                </> : <>
                  <button className="primary-button" disabled={!dashboard.telegramConnected} onClick={() => void handleBulkDownload()}><Download size={15} /> Descargar selección</button>
                  <button className="secondary-button" disabled={!dashboard.telegramConnected} onClick={() => setMoveDialog({ kind: "files", ids: [...selectedFiles] })}><Move size={15} /> Mover a…</button>
                  <button className="secondary-button" onClick={() => void handleBulkTrash([...selectedFiles], true)}><Trash2 size={15} /> Papelera</button>
                </>}
                <button className="secondary-button" onClick={() => setSelectedFiles(new Set())}>Limpiar</button>
              </>}
              {section !== "trash" && <label className="conflict-control">Subidas simultáneas
                <select aria-label="Subidas simultáneas" value={dashboard.settings.uploadConcurrency} onChange={(event) => void action(() => updateSetting("upload_concurrency", event.target.value))}>
                  {[1, 2, 4, 6, 8, 12, 16].map(value => <option key={value} value={value}>{value}</option>)}
                </select>
              </label>}
              {section !== "trash" && <label className="conflict-control">Si ya existe
                <select value={dashboard.settings.conflictPolicy} onChange={(event) => void action(() => updateSetting("conflict_policy", event.target.value))}>
                  <option value="skip">Omitir</option><option value="rename">Crear nombre alternativo</option>
                </select>
              </label>}
            </div>

            <div className="file-toolbar">
              <div className="file-search-bar" role="search">
                <Search size={15} />
                <input
                  ref={fileSearchInputRef}
                  type="text"
                  value={query}
                  onChange={(event) => setQuery(event.target.value)}
                  placeholder="Buscar archivo por nombre…"
                  aria-label="Buscar archivo por nombre"
                />
                {query && (
                  <button
                    className="clear-search-btn"
                    type="button"
                    onClick={() => setQuery("")}
                    aria-label="Limpiar búsqueda"
                    title="Limpiar búsqueda"
                  >
                    <X size={13} />
                  </button>
                )}
              </div>
              <div className="file-type-strip">
                <button className="filter-scroll-button" type="button" onClick={() => scrollFileTypes(-1)} aria-label="Ver tipos anteriores"><ChevronLeft size={17} /></button>
                <div ref={filterTabsRef} className="filter-tabs" role="group" aria-label="Tipo de archivo">{filterItems.map((item) => <button key={item.key} aria-pressed={fileFilter === item.key} className={fileFilter === item.key ? "active" : ""} onClick={() => setFileFilter(item.key)}>{item.label}</button>)}</div>
                <button className="filter-scroll-button" type="button" onClick={() => scrollFileTypes(1)} aria-label="Ver más tipos"><ChevronRight size={17} /></button>
              </div>
              <div className="toolbar-actions">
                <div className="sort-select-wrap"><SlidersHorizontal size={15} /><select value={sort} onChange={(event) => setSort(event.target.value as SortKey)} aria-label="Ordenar"><option value="recent">Más reciente</option><option value="oldest">Más antiguo</option><option value="name">Nombre</option><option value="size">Tamaño</option></select></div>
                <button className={`icon-button ${filterPanel ? "active" : ""}`} onClick={() => setFilterPanel((value) => !value)} aria-label="Filtros"><Filter size={17} /></button>
                <div className="view-toggle"><button className={view === "grid" ? "active" : ""} onClick={() => setView("grid")} aria-label="Cuadrícula"><Grid2X2 size={16} /></button><button className={view === "list" ? "active" : ""} onClick={() => setView("list")} aria-label="Lista"><List size={17} /></button></div>
              </div>
            </div>

            {filterPanel && <div className="advanced-filter-card"><div><span className="filter-title">Etiqueta</span><div className="compact-chip-row"><button className={!selectedTag ? "active" : ""} onClick={() => setSelectedTag(null)}>Todas</button>{tagOptions.map((tag) => <button className={selectedTag === tag ? "active" : ""} key={tag} onClick={() => setSelectedTag(tag)}>{tag}</button>)}</div></div><button className="clear-filter" onClick={() => { setSelectedTag(null); setFileFilter("all"); setQuery(""); }}>Limpiar filtros</button></div>}

            {section === "files" && visibleFolders.length > 0 && <div className="folder-grid">{visibleFolders.map((folder) => <FolderCard
              key={folder.id}
              folder={folder}
              dropActive={dragTarget === folder.id}
              onOpen={() => { setCurrentFolderId(folder.id); setSelectedFiles(new Set()); }}
              onRename={() => setFolderEditor({ mode: "rename", folder })}
              onMove={() => setMoveDialog({ kind: "folder", folder })}
              onDelete={() => void handleDeleteFolder(folder)}
              onFileDragOver={(event) => {
                event.preventDefault();
                event.stopPropagation();
                event.dataTransfer.dropEffect = "move";
                if (dragTarget !== folder.id) setDragTarget(folder.id);
              }}
              onFileDragLeave={(event) => {
                if (event.currentTarget.contains(event.relatedTarget as Node)) return;
                if (dragTarget === folder.id) setDragTarget(null);
              }}
              onFileDrop={(event) => {
                event.preventDefault();
                event.stopPropagation();
                desktopDrop(event, folder.id);
              }}
            />)}</div>}

            {files.length === 0 && visibleFolders.length === 0 ? (
              <div className="empty-state">
                <div className="empty-icon"><Search size={25} /></div>
                <h3>{query ? "No se encontraron resultados" : "Todavía no hay archivos aquí"}</h3>
                <p>
                  {query
                    ? `No hay ningún archivo ni carpeta que coincida con “${query}”. Revisa el término o limpia la búsqueda.`
                    : section === "files"
                      ? "Crea una carpeta o sube archivos en esta ubicación."
                      : "Conecta Telegram y sube tu primer archivo, o revisa los filtros."}
                </p>
                {query && (
                  <button className="secondary-button" type="button" onClick={() => setQuery("")}>
                    Limpiar búsqueda
                  </button>
                )}
              </div>
            ) : files.length === 0 ? null : view === "grid" ? (
              <>
                <div className="file-grid">{visibleFiles.map((file) => <FileCard key={file.id} file={file} selected={selectedFiles.has(file.id)} isDragging={draggingFileIds.includes(file.id)} onSelect={() => toggleSelection(file.id)} onFavorite={() => void action(() => setFavorite(file.id, !file.favorite))} onDownload={() => void handleBulkDownload([file.id])} onPreview={() => void handleMedia(file)} onMove={() => setMoveDialog({ kind: "files", ids: [file.id] })} onTrash={() => void action(() => setTrashed(file.id, !file.trashed))} onDelete={() => void handlePermanentDelete([file.id])} onDragStart={(event) => desktopDragStart(event, file)} onDragEnd={clearFileDrag} onPointerDown={(event) => touchDragStart(event, file)} onPointerMove={touchDragMove} onPointerUp={touchDragEnd} onPointerCancel={() => { if (touchDragRef.current) clearFileDrag(); }} />)}</div>
                {visibleCount < files.length && (
                  <div ref={loadMoreSentinelRef} className="load-more-sentinel" style={{ padding: "16px 0", textAlign: "center" }}>
                    <button className="secondary-button" type="button" onClick={() => setVisibleCount((prev) => Math.min(prev + 80, files.length))}>
                      Cargar más ({visibleFiles.length} de {files.length})
                    </button>
                  </div>
                )}
              </>
            ) : (
              <>
                <div className="file-list"><div className="file-list-head"><span>Nombre</span><span>Ubicación</span><span>Tamaño</span><span>Modificado</span><span /></div>{visibleFiles.map((file) => <FileRow key={file.id} file={file} selected={selectedFiles.has(file.id)} isDragging={draggingFileIds.includes(file.id)} onSelect={() => toggleSelection(file.id)} onFavorite={() => void action(() => setFavorite(file.id, !file.favorite))} onDownload={() => void handleBulkDownload([file.id])} onPreview={() => void handleMedia(file)} onMove={() => setMoveDialog({ kind: "files", ids: [file.id] })} onTrash={() => void action(() => setTrashed(file.id, !file.trashed))} onDelete={() => void handlePermanentDelete([file.id])} onDragStart={(event) => desktopDragStart(event, file)} onDragEnd={clearFileDrag} onPointerDown={(event) => touchDragStart(event, file)} onPointerMove={touchDragMove} onPointerUp={touchDragEnd} onPointerCancel={() => { if (touchDragRef.current) clearFileDrag(); }} />)}</div>
                {visibleCount < files.length && (
                  <div ref={loadMoreSentinelRef} className="load-more-sentinel" style={{ padding: "16px 0", textAlign: "center" }}>
                    <button className="secondary-button" type="button" onClick={() => setVisibleCount((prev) => Math.min(prev + 80, files.length))}>
                      Cargar más ({visibleFiles.length} de {files.length})
                    </button>
                  </div>
                )}
              </>
            )}
          </section>}
        </div>
      </main>

      {/* Barra de estado / píldora de transferencias para Windows y Android */}
      {(queue.total > 0 || queue.active > 0 || queue.completed > 0 || queue.failed > 0 || skippedFiles.length > 0) && (
        <div
          className="transfer-status-bar"
          role="status"
          aria-label="Contador de transferencias completadas y no completadas"
          onClick={() => setSection(queue.total > 0 ? "home" : "history")}
        >
          <div className="transfer-status-pills">
            <span className="transfer-pill completed" title="Archivos completados con éxito">
              <CheckCircle2 size={15} />
              <strong>{queue.completed}</strong>
              <span className="pill-text">completados</span>
            </span>
            <span
              className={`transfer-pill ${queue.failed + queue.pending + skippedFiles.length > 0 ? "uncompleted" : "idle"}`}
              title="Transferencias no completadas (en cola, con error u omitidas)"
            >
              {queue.failed + queue.pending + skippedFiles.length > 0 ? (
                <AlertCircle size={15} />
              ) : (
                <CheckCircle2 size={15} />
              )}
              <strong>{queue.failed + queue.pending + skippedFiles.length}</strong>
              <span className="pill-text">no completados</span>
              {(queue.failed > 0 || queue.pending > 0 || skippedFiles.length > 0) && (
                <span className="pill-detail">
                  ({[
                    queue.failed > 0 ? `${queue.failed} error` : null,
                    queue.pending > 0 ? `${queue.pending} en espera` : null,
                    skippedFiles.length > 0 ? `${skippedFiles.length} omitidos` : null,
                  ].filter(Boolean).join(", ")})
                </span>
              )}
            </span>
          </div>
          {queue.active > 0 && (
            <div className="transfer-live-rate">
              <span className="transfer-pulse" />
              <span>{formatSpeed(queue.speedBps)}</span>
            </div>
          )}
          <button
            type="button"
            className="transfer-bar-action"
            onClick={(e) => {
              e.stopPropagation();
              setSection(queue.total > 0 ? "home" : "history");
            }}
          >
            {queue.total > 0 ? "Ver cola" : "Historial"} →
          </button>
        </div>
      )}

      {/* Indicador visual de arrastre de carpetas o archivos desde el sistema operativo */}
      {externalDragActive && (
        <div className="external-drag-scrim" role="region" aria-label="Soltar archivos en Nuvio">
          <div className="external-drag-card">
            <div className="external-drag-icon-pulse">
              <FolderUp size={36} />
            </div>
            <h3>Suelta aquí para subir a Nuvio</h3>
            <p>
              Destino: <strong>{externalDragTargetName}</strong>
            </p>
            <div className="external-drag-badge">
              <span>Subida recursiva de carpetas · {dashboard.settings.uploadConcurrency} en simultáneo</span>
            </div>
          </div>
        </div>
      )}

      <nav className="mobile-bottom-nav" aria-label="Navegación móvil">{navItems.slice(0, 4).map((item) => { const Icon = item.icon; return <button key={item.key} className={section === item.key ? "active" : ""} onClick={() => { if (item.key === "files") setCurrentFolderId(null); setSection(item.key); }}><Icon size={19} /><span>{item.label === "Mis archivos" ? "Archivos" : item.label}</span></button>; })}<button onClick={() => setMobileMenu(true)}><Menu size={20} /><span>Más</span></button></nav>

      {touchDragPosition && draggingFileIds.length > 0 && (
        <div
          className={`touch-drag-badge ${dragTarget ? "has-target" : ""}`}
          style={{ left: touchDragPosition.x, top: touchDragPosition.y }}
          aria-hidden="true"
        >
          <Move size={16} />
          <strong>{draggingFileIds.length}</strong>
          <span>
            {dragTarget
              ? dragTarget === "__root__"
                ? "Mover a Mi unidad"
                : `Mover a ${dashboard?.folders.find((f) => f.id === dragTarget)?.name ?? "carpeta"}`
              : draggingFileIds.length === 1
              ? "archivo"
              : "archivos"}
          </span>
        </div>
      )}

      {folderEditor && <FolderEditorDialog
        mode={folderEditor.mode}
        initialName={folderEditor.folder?.name ?? ""}
        parentName={currentFolder?.name ?? "Mi unidad"}
        onClose={() => setFolderEditor(null)}
        onSave={handleSaveFolder}
      />}

      {moveDialog && <MoveToFolderDialog
        folders={dashboard.folders}
        movingFolderId={moveDialog.kind === "folder" ? moveDialog.folder.id : null}
        itemLabel={moveDialog.kind === "folder" ? `la carpeta “${moveDialog.folder.name}”` : `${moveDialog.ids.length} archivo${moveDialog.ids.length === 1 ? "" : "s"}`}
        onClose={() => setMoveDialog(null)}
        onMove={handleMoveConfirm}
      />}

      {mediaFile && <Dialog className="media-modal" label={`Vista previa ${mediaFile.name}`} onClose={closeMedia}>
        <button className="icon-button modal-close" onClick={closeMedia} aria-label="Cerrar vista previa"><X size={18} /></button>
        <div className="media-modal-header"><div className="modal-eyebrow">Vista previa · caché privada</div><h2>{fileName(mediaFile)}</h2><p>Nuvio prepara una copia temporal privada para mostrar el archivo sin guardarlo en tu carpeta de descargas.</p></div>
        {mediaBusy && <div className="media-preparing" role="status"><RefreshCw size={22} /><strong>Preparando vista previa…</strong><span>Puedes cerrar esta ventana; la preparación continuará en segundo plano.</span></div>}
        {mediaError && <div className="modal-error" role="alert">{mediaError}</div>}
        {mediaSrc && mediaFile.kind === "image" && <img className="image-preview" src={mediaSrc} alt={fileName(mediaFile)} onError={() => setMediaError("Esta imagen no se puede mostrar aquí. Puedes descargarla para abrirla con otra aplicación.")} />}
        {mediaSrc && mediaFile.kind === "pdf" && <PdfPreview source={mediaSrc} onError={setMediaError} />}
        {previewText !== null && (mediaFile.kind === "text" || mediaFile.kind === "code") && <pre className="text-preview">{previewText}</pre>}
        {mediaSrc && mediaFile.kind === "video" && <video className="media-player" src={mediaSrc} controls autoPlay onError={() => setMediaError("Este video no se puede reproducir aquí. Descárgalo para abrirlo con otro reproductor.")} />}
        {mediaSrc && mediaFile.kind === "audio" && <audio className="audio-player" src={mediaSrc} controls autoPlay onError={() => setMediaError("Este audio no se puede reproducir aquí. Descárgalo para abrirlo con otro reproductor.")} />}
      </Dialog>}

      {connectModal && <TelegramConnectModal rememberDefault={dashboard.settings.rememberSession} onClose={() => setConnectModal(false)} onChanged={async (snapshot) => { await refreshDashboard(); if (snapshot.connected) setAppNotice(`Telegram conectado${snapshot.accountLabel ? ` · ${snapshot.accountLabel}` : ""}`); }} />}

      {showSkippedModal && skippedFiles.length > 0 && (
        <SkippedUploadsModal
          items={skippedFiles}
          isPremium={Boolean(dashboard.isPremium)}
          onClose={() => setShowSkippedModal(false)}
        />
      )}
    </div>
  );
}

function FolderCard({ folder, dropActive, onOpen, onRename, onMove, onDelete, onFileDragOver, onFileDragLeave, onFileDrop }: {
  folder: CloudFolder;
  dropActive: boolean;
  onOpen: () => void;
  onRename: () => void;
  onMove: () => void;
  onDelete: () => void;
  onFileDragOver: (event: React.DragEvent<HTMLElement>) => void;
  onFileDragLeave: (event: React.DragEvent<HTMLElement>) => void;
  onFileDrop: (event: React.DragEvent<HTMLElement>) => void;
}) {
  const empty = folder.fileCount === 0 && folder.childCount === 0;
  return (
    <article
      data-nuvio-drop-folder={folder.id}
      className={`folder-card ${dropActive ? "drag-over" : ""}`}
      onDragOver={onFileDragOver}
      onDragEnter={onFileDragOver}
      onDragLeave={onFileDragLeave}
      onDrop={onFileDrop}
    >
      <button
        className="folder-open-area"
        onClick={onOpen}
        aria-label={`Abrir ${folder.name}`}
        onDragEnter={onFileDragOver}
        onDragOver={onFileDragOver}
        onDragLeave={onFileDragLeave}
        onDrop={onFileDrop}
      >
        <span className="folder-icon"><Folder size={23} fill="currentColor" /></span>
        <span className="folder-copy"><strong>{folder.name}</strong><small>{folder.fileCount} archivo{folder.fileCount === 1 ? "" : "s"} · {folder.childCount} carpeta{folder.childCount === 1 ? "" : "s"}</small></span>
        <ChevronRight size={16} />
      </button>
      <div className="folder-actions">
        <button className="ghost-icon" onClick={onRename} title="Renombrar"><Pencil size={15} /></button>
        <button className="ghost-icon" onClick={onMove} title="Mover carpeta"><Move size={15} /></button>
        <button className="ghost-icon danger" disabled={!empty} onClick={onDelete} title={empty ? "Eliminar carpeta" : "Vacía la carpeta antes de eliminarla"}><Trash2 size={15} /></button>
      </div>
    </article>
  );
}

function FolderEditorDialog({ mode, initialName, parentName, onClose, onSave }: { mode: "create" | "rename"; initialName: string; parentName: string; onClose: () => void; onSave: (name: string) => Promise<void> }) {
  const [name, setName] = useState(initialName);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    const normalized = name.trim();
    if (!normalized) return setError("Escribe un nombre para la carpeta");
    setBusy(true); setError(null);
    try { await onSave(normalized); } catch (value) { setError(readableError(value)); } finally { setBusy(false); }
  };
  return <Dialog className="folder-modal" label={mode === "create" ? "Nueva carpeta" : "Renombrar carpeta"} onClose={onClose}>
    <button className="icon-button modal-close" onClick={onClose} aria-label="Cerrar"><X size={18} /></button>
    <div className="modal-brand"><FolderPlus size={23} /></div><div className="modal-eyebrow">{parentName}</div><h2>{mode === "create" ? "Nueva carpeta" : "Renombrar carpeta"}</h2>
    <p>{mode === "create" ? "La estructura se sincronizará con Telegram y aparecerá igual en Windows y Android." : "El nuevo nombre se sincronizará en tus dispositivos."}</p>
    {error && <div className="modal-error" role="alert">{error}</div>}
    <form className="credential-placeholder" onSubmit={submit}><label htmlFor="folder-name">Nombre</label><input id="folder-name" autoFocus maxLength={120} value={name} onChange={(event) => setName(event.target.value)} /><button className="primary-button modal-primary" disabled={busy} type="submit">{busy ? "Guardando…" : mode === "create" ? "Crear carpeta" : "Guardar nombre"}</button></form>
  </Dialog>;
}

function folderPathLabel(folder: CloudFolder, folders: CloudFolder[]): string {
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

function MoveToFolderDialog({ folders, movingFolderId, itemLabel, onClose, onMove }: { folders: CloudFolder[]; movingFolderId: string | null; itemLabel: string; onClose: () => void; onMove: (folderId: string | null) => Promise<void> }) {
  const [target, setTarget] = useState("__root__");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const choices = folders.filter((folder) => !folder.trashed && folder.id !== movingFolderId).sort((a, b) => folderPathLabel(a, folders).localeCompare(folderPathLabel(b, folders), "es", { numeric: true, sensitivity: "base" }));
  const submit = async (event: React.FormEvent) => {
    event.preventDefault(); setBusy(true); setError(null);
    try { await onMove(target === "__root__" ? null : target); } catch (value) { setError(readableError(value)); } finally { setBusy(false); }
  };
  return <Dialog className="folder-modal" label="Mover a carpeta" onClose={onClose}>
    <button className="icon-button modal-close" onClick={onClose} aria-label="Cerrar"><X size={18} /></button>
    <div className="modal-brand"><FolderOpen size={23} /></div><div className="modal-eyebrow">Organización Nuvio</div><h2>Mover a…</h2><p>Elige el destino para {itemLabel}. El cambio se sincronizará mediante Telegram.</p>
    {error && <div className="modal-error" role="alert">{error}</div>}
    <form className="credential-placeholder" onSubmit={submit}><label htmlFor="folder-target">Destino</label><select id="folder-target" value={target} onChange={(event) => setTarget(event.target.value)}><option value="__root__">Mi unidad</option>{choices.map((folder) => <option key={folder.id} value={folder.id}>{folderPathLabel(folder, folders)}</option>)}</select><button className="primary-button modal-primary" disabled={busy} type="submit">{busy ? "Moviendo…" : "Mover"}</button></form>
  </Dialog>;
}

function TransferRow({ job, connected, onAction }: { job: TransferJob; connected: boolean; onAction: (operation: () => Promise<unknown>) => void }) {
  const calculating = ["analyzing", "copying", "uploading", "downloading", "confirming", "running"].includes(job.status);
  return <article className={`transfer-detail status-${job.status}`}>
    <div className="transfer-direction">{job.direction === "upload" ? <ArrowUpFromLine size={16} /> : <ArrowDownToLine size={16} />}</div>
    <div className="transfer-main">
      <div className="transfer-title-line"><strong>{job.fileName}</strong><span className="phase-badge">{phaseLabel(job)}</span></div>
      <div className="transfer-stats"><span>{formatBytes(job.processedBytes)} / {formatBytes(job.totalBytes)}</span><span>{formatSpeed(job.speedBps)}</span><span>{formatEta(job.etaSeconds, calculating && job.speedBps <= 0)}</span>{job.attempts > 0 && <span>Intento {Math.min(job.attempts + 1, job.maxAttempts)}/{job.maxAttempts}</span>}</div>
      <progress max={100} value={job.progress} aria-label={`Progreso ${job.fileName}`} />
      {job.error && <p className="transfer-error">{job.error}</p>}
    </div>
    <div className="transfer-controls">
      {job.canPause && <button className="ghost-icon" onClick={() => onAction(() => pauseTransfer(job.id))} title="Pausar"><Pause size={15} /></button>}
      {(job.status === "paused" || job.status === "cancelled") && <button className="ghost-icon" disabled={!connected && job.direction === "upload"} onClick={() => onAction(() => resumeTransfer(job.id))} title="Continuar"><Play size={15} /></button>}
      {job.status === "failed" && <button className="ghost-icon" disabled={!connected} onClick={() => onAction(() => retryTransfer(job.id))} title="Reintentar"><RotateCcw size={15} /></button>}
      {job.canCancel && <button className="ghost-icon danger" onClick={() => onAction(() => cancelTransfer(job.id))} title="Cancelar"><X size={15} /></button>}
      {job.status === "completed" && <Check size={18} className="success-icon" />}
    </div>
  </article>;
}

function TransferHistoryRow({ job }: { job: TransferJob }) {
  const terminalLabel = job.status === "completed"
    ? "Completado"
    : job.status === "duplicate"
      ? "Duplicado"
      : job.status === "cancelled"
        ? "Cancelado"
        : phaseLabel(job);
  return <article className={`history-row status-${job.status}`}>
    <div className="transfer-direction">{job.direction === "upload" ? <ArrowUpFromLine size={16} /> : <ArrowDownToLine size={16} />}</div>
    <div className="history-row-copy"><strong>{job.fileName}</strong><span>{job.direction === "upload" ? "Subida" : "Descarga"} · {formatBytes(job.totalBytes)}</span></div>
    <span className="phase-badge">{terminalLabel}</span>
    <time>{transferDate(job.updatedAt)}</time>
  </article>;
}

function PdfPreview({ source, onError }: { source: string; onError: (message: string) => void }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    const task = getDocument({ url: source });
    void (async () => {
      try {
        const pdf = await task.promise;
        const page = await pdf.getPage(1);
        if (cancelled) return;
        const base = page.getViewport({ scale: 1 });
        const scale = Math.min(1.45, 4096 / base.width, 4096 / base.height, Math.sqrt(4_000_000 / (base.width * base.height)));
        const viewport = page.getViewport({ scale });
        const canvas = canvasRef.current;
        const context = canvas?.getContext("2d");
        if (!canvas || !context) throw new Error("Canvas no disponible");
        canvas.width = Math.ceil(viewport.width);
        canvas.height = Math.ceil(viewport.height);
        await page.render({ canvas, canvasContext: context, viewport }).promise;
        if (!cancelled) setLoading(false);
      } catch {
        if (!cancelled) {
          setLoading(false);
          onError("No se pudo generar la vista previa del PDF. Puedes descargarlo para abrirlo completo.");
        }
      }
    })();
    return () => {
      cancelled = true;
      void task.destroy();
    };
  }, [source, onError]);

  return <div className="pdf-preview">{loading && <div className="preview-loading"><RefreshCw size={19} /> Renderizando primera página…</div>}<canvas ref={canvasRef} /></div>;
}

function TelegramConnectModal({ rememberDefault, onClose, onChanged }: { rememberDefault: boolean; onClose: () => void; onChanged: (snapshot: TelegramAuthSnapshot) => void | Promise<void> }) {
  const [snapshot, setSnapshot] = useState<TelegramAuthSnapshot | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [rememberSession, setRememberSession] = useState(rememberDefault);
  const [apiId, setApiId] = useState("");
  const [apiHash, setApiHash] = useState("");
  const [phone, setPhone] = useState("");
  const [editingPhone, setEditingPhone] = useState(false);
  const [resendCooldown, setResendCooldown] = useState(0);
  const [resendNotice, setResendNotice] = useState<string | null>(null);
  const [email, setEmail] = useState("");
  const [code, setCode] = useState("");
  const [password, setPassword] = useState("");
  const [firstName, setFirstName] = useState("");
  const [lastName, setLastName] = useState("");

  const applySnapshot = async (next: TelegramAuthSnapshot) => { setSnapshot(next); setError(null); await onChanged(next); };
  const refresh = async () => { try { await applySnapshot(await getTelegramAuthState()); } catch (value) { setError(readableError(value)); } };
  useEffect(() => { void refresh(); }, []);
  useEffect(() => {
    const stages = new Set(["initializing", "qr", "loggingOut", "closing"]);
    if (!snapshot || !stages.has(snapshot.stage)) return;
    const timer = window.setInterval(() => void refresh(), 1400);
    return () => window.clearInterval(timer);
  }, [snapshot?.stage]);

  useEffect(() => {
    if (resendCooldown <= 0) return;
    const timer = window.setInterval(() => setResendCooldown((prev) => Math.max(0, prev - 1)), 1000);
    return () => window.clearInterval(timer);
  }, [resendCooldown]);

  const run = async (operation: () => Promise<TelegramAuthSnapshot>, clear?: () => void) => {
    if (busy) return;
    setBusy(true); setError(null);
    try { await applySnapshot(await operation()); } catch (value) { setError(readableError(value)); } finally { clear?.(); setBusy(false); }
  };

  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    if (!snapshot || busy) return;
    switch (snapshot.stage) {
      case "needsCredentials": {
        const id = Number(apiId.trim());
        if (!Number.isInteger(id) || id <= 0 || id > 2147483647) return setError("API ID debe ser un número entero entre 1 y 2147483647");
        if (!/^[a-fA-F0-9]{32}$/.test(apiHash.trim())) return setError("API Hash no parece válido");
        void run(() => configureTelegram(id, apiHash.trim(), rememberSession), () => setApiHash("")); break;
      }
      case "phone": void run(() => submitTelegramPhone(phone.trim())); break;
      case "email": void run(() => submitTelegramEmail(email.trim())); break;
      case "emailCode": void run(() => submitTelegramEmailCode(code), () => setCode("")); break;
      case "code": void run(() => submitTelegramCode(code), () => setCode("")); break;
      case "password": void run(() => submitTelegramPassword(password), () => setPassword("")); break;
      case "registration": void run(() => registerTelegramUser(firstName, lastName), () => { setFirstName(""); setLastName(""); }); break;
      default: void refresh();
    }
  };

  const stage = snapshot?.stage;
  return <Dialog className="connect-modal" labelledBy="connect-title" onClose={onClose}>
    <button className="icon-button modal-close" onClick={onClose} aria-label="Cerrar"><X size={18} /></button>
    <div className="modal-brand"><Cloud size={24} /></div><div className="modal-eyebrow">Proveedor remoto</div><h2 id="connect-title">{snapshot?.connected ? "Telegram conectado" : "Conectar Telegram"}</h2><p>{snapshot?.message ?? "Consultando la conexión de Telegram…"}</p>
    <div className="security-note"><div><Check size={16} /><span>Tu propia cuenta</span></div><div><Check size={16} /><span>Mensajes guardados</span></div><div><Check size={16} /><span>Secretos fuera de logs</span></div></div>
    {error && <div className="modal-error" role="alert">{error}</div>}
    {!snapshot ? <div className="auth-loading">Conectando con Telegram…</div> : snapshot.connected || stage === "ready" ? <div className="auth-success account-connected-panel">
      <div className="auth-success-icon"><Check size={20} /></div><div><strong>{snapshot.accountLabel ?? "Cuenta de Telegram"}</strong><span>{snapshot.isPremium ? "Telegram Premium" : "Cuenta estándar"}</span><small>{rememberDefault ? "Esta sesión está marcada para recordarse en este equipo." : "La sesión no se guardará como credencial reutilizable."}</small></div>
      <div className="account-actions"><button className="secondary-button" disabled={busy} onClick={() => void run(logOutTelegram)}>Cerrar sesión</button><button className="secondary-button danger-button" disabled={busy} onClick={() => void run(forgetTelegramSession)}>Olvidar sesión</button></div>
    </div> : stage === "closed" ? <div className="auth-closed"><strong>Sesión cerrada</strong><p>{snapshot.message}</p></div> : <form className="credential-placeholder" onSubmit={submit}>
      {stage === "needsCredentials" && <><label htmlFor="telegram-api-id">API ID</label><input id="telegram-api-id" inputMode="numeric" value={apiId} onChange={(event) => setApiId(event.target.value)} autoComplete="off" /><label htmlFor="telegram-api-hash">API Hash</label><input id="telegram-api-hash" type="password" value={apiHash} onChange={(event) => setApiHash(event.target.value)} autoComplete="off" /><label className="remember-session-control"><input type="checkbox" checked={rememberSession} onChange={(event) => setRememberSession(event.target.checked)} /><span><strong>Recordar sesión en este equipo</strong><small>Desactivado por defecto. Las credenciales se protegen con el almacén seguro de este dispositivo.</small></span></label></>}
      {stage === "phone" && <><label htmlFor="telegram-phone">Teléfono</label><input id="telegram-phone" type="tel" value={phone} onChange={(event) => setPhone(event.target.value)} placeholder="+52XXXXXXXXXX" autoComplete="tel" autoFocus /></>}
      {stage === "email" && <><label htmlFor="telegram-email">Correo de autenticación</label><input id="telegram-email" type="email" value={email} onChange={(event) => setEmail(event.target.value)} autoComplete="email" /></>}
      {(stage === "code" || stage === "emailCode") && (
        editingPhone ? (
          <div className="auth-correct-phone-container">
            <label htmlFor="telegram-phone-edit">Corregir o cambiar número de teléfono</label>
            <span className="auth-hint">Ingresa tu número correcto con código de país (ej. +521234567890)</span>
            <input
              id="telegram-phone-edit"
              type="tel"
              value={phone}
              onChange={(event) => setPhone(event.target.value)}
              placeholder="+52XXXXXXXXXX"
              autoComplete="tel"
              autoFocus
            />
            <div className="auth-recovery-actions">
              <button
                className="primary-button modal-primary"
                type="button"
                disabled={busy}
                onClick={() => {
                  if (!phone.trim().startsWith("+") || phone.trim().length < 8) {
                    return setError("Usa el número en formato internacional, por ejemplo +52 seguido de tu número");
                  }
                  void run(
                    () => submitTelegramPhone(phone.trim()),
                    () => {
                      setEditingPhone(false);
                      setCode("");
                      setResendNotice("Código solicitado al número corregido.");
                    },
                  );
                }}
              >
                {busy ? "Enviando…" : "Reenviar código al nuevo número"}
              </button>
              <button
                className="secondary-button modal-secondary"
                type="button"
                disabled={busy}
                onClick={() => {
                  setEditingPhone(false);
                  setError(null);
                }}
              >
                Cancelar y volver a ingresar código
              </button>
            </div>
          </div>
        ) : (
          <>
            <label htmlFor="telegram-code">{stage === "emailCode" ? "Código de correo" : "Código de Telegram"}</label>
            {snapshot.hint && <span className="auth-hint">{snapshot.hint}</span>}
            <input id="telegram-code" inputMode="numeric" value={code} onChange={(event) => setCode(event.target.value)} autoComplete="one-time-code" autoFocus />
            <div className="auth-recovery-buttons">
              <button
                type="button"
                className="auth-link-button"
                disabled={busy}
                onClick={() => {
                  setEditingPhone(true);
                  setError(null);
                  setResendNotice(null);
                }}
              >
                ¿Te equivocaste de número? Corregir número
              </button>
              <button
                type="button"
                className="auth-link-button"
                disabled={busy || resendCooldown > 0}
                onClick={async () => {
                  if (busy || resendCooldown > 0) return;
                  try {
                    setBusy(true);
                    setError(null);
                    if (stage === "code") {
                      await applySnapshot(await resendTelegramCode());
                    } else {
                      await applySnapshot(await submitTelegramEmail(email));
                    }
                    setResendNotice("Se ha solicitado el reenvío del código.");
                    setResendCooldown(30);
                  } catch (value) {
                    setError(readableError(value));
                  } finally {
                    setBusy(false);
                  }
                }}
              >
                {resendCooldown > 0 ? `Reenviar código (${resendCooldown}s)` : "¿No te llega el código? Reenviar código"}
              </button>
            </div>
            {resendNotice && <div className="auth-resend-notice">{resendNotice}</div>}
          </>
        )
      )}
      {stage === "password" && <><label htmlFor="telegram-password">Contraseña 2FA</label>{snapshot.hint && <span className="auth-hint">Pista: {snapshot.hint}</span>}<input id="telegram-password" type="password" value={password} onChange={(event) => setPassword(event.target.value)} autoComplete="current-password" /></>}
      {stage === "registration" && <><label htmlFor="telegram-first-name">Nombre</label><input id="telegram-first-name" value={firstName} onChange={(event) => setFirstName(event.target.value)} /><label htmlFor="telegram-last-name">Apellido</label><input id="telegram-last-name" value={lastName} onChange={(event) => setLastName(event.target.value)} /></>}
      {stage === "qr" && <div className="qr-auth-card"><strong>Autoriza desde Telegram</strong><span>Telegram → Ajustes → Dispositivos → Vincular dispositivo de escritorio.</span>{snapshot.qrSvg && <img className="auth-qr" alt="QR de Telegram" src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(snapshot.qrSvg)}`} />}</div>}
      {(stage === "initializing" || stage === "loggingOut" || stage === "closing") && <div className="auth-loading">{snapshot.message}</div>}
      {!editingPhone && !new Set(["initializing", "qr", "loggingOut", "closing"]).has(stage ?? "") && <button className="primary-button modal-primary" type="submit" disabled={busy}>{busy ? "Procesando…" : authActionLabel(stage)}</button>}
      {stage === "phone" && <button className="secondary-button modal-secondary" type="button" disabled={busy} onClick={() => void run(requestTelegramQr)}>Usar otra sesión / QR</button>}
    </form>}
    <small>Los códigos, el teléfono y la contraseña 2FA no se guardan en los logs de Nuvio.</small>
  </Dialog>;
}

function authActionLabel(stage?: TelegramAuthSnapshot["stage"]): string {
  switch (stage) {
    case "needsCredentials": return "Continuar con Telegram";
    case "phone": return "Enviar código";
    case "email": return "Continuar";
    case "emailCode": return "Verificar correo";
    case "code": return "Verificar código";
    case "password": return "Verificar contraseña";
    case "registration": return "Completar registro";
    default: return "Continuar";
  }
}

function SkippedUploadsModal({
  items,
  isPremium,
  onClose,
}: {
  items: SkippedUploadItem[];
  isPremium: boolean;
  onClose: () => void;
}) {
  const [copied, setCopied] = useState(false);

  const copyList = async () => {
    try {
      const text = items
        .map(
          (item, idx) =>
            `${idx + 1}. ${item.fileName}\n   Ruta: ${item.path}\n   Motivo: ${item.reason}${
              item.sizeBytes ? ` (${formatBytes(item.sizeBytes)})` : ""
            }`,
        )
        .join("\n\n");
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2500);
    } catch {
      // ignore
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
          {isPremium ? " (con tu suscripción Telegram Premium)" : " (o 4 GB con Telegram Premium)"}.
          Nuvio omitió los siguientes archivos para que el resto de tu contenido continúe subiéndose con normalidad.
        </p>
      </div>

      <div className="skipped-list-container" role="region" aria-label="Lista de archivos omitidos">
        {items.map((item, idx) => (
          <div key={`${item.path}-${idx}`} className="skipped-item-card">
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

type FileActions = {
  file: CloudFile;
  selected: boolean;
  isDragging?: boolean;
  onSelect: () => void;
  onFavorite: () => void;
  onDownload: () => void;
  onPreview: () => void;
  onMove: () => void;
  onTrash: () => void;
  onDelete: () => void;
  onDragStart: (event: React.DragEvent<HTMLElement>) => void;
  onDragEnd: () => void;
  onPointerDown: (event: React.PointerEvent<HTMLElement>) => void;
  onPointerMove: (event: React.PointerEvent<HTMLElement>) => void;
  onPointerUp: (event: React.PointerEvent<HTMLElement>) => void;
  onPointerCancel: () => void;
};

const FileCard = memo(function FileCard({ file, selected, isDragging, onSelect, onFavorite, onDownload, onPreview, onMove, onTrash, onDelete, onDragStart, onDragEnd, onPointerDown, onPointerMove, onPointerUp, onPointerCancel }: FileActions) {
  const Icon = kindIcon(file.kind);
  return <article className={`file-card ${selected ? "selected" : ""} ${isDragging ? "is-touch-dragging" : ""}`} draggable={false} tabIndex={0} aria-label={fileName(file)} onClick={(event) => { if (!(event.target as Element).closest("button,input,label,select,a")) onSelect(); }} onKeyDown={(event) => { if (event.target === event.currentTarget && event.key === " ") { event.preventDefault(); onSelect(); } }} onDragStart={onDragStart} onDragEnd={onDragEnd} onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={onPointerCancel}>
    <div className={`file-preview kind-${file.kind}`}>
      <label className="file-select-checkbox"><input type="checkbox" checked={selected} onChange={onSelect} aria-label={`Seleccionar ${file.name}`} /></label>
      <FileThumbnail file={file}><div className="file-type-icon"><Icon size={27} strokeWidth={1.7} /></div></FileThumbnail>
      <span className="file-extension">{file.extension.toUpperCase()}</span>
      <button className={`favorite-button ${file.favorite ? "active" : ""}`} onClick={onFavorite} aria-label="Favorito" aria-pressed={file.favorite}><Star size={16} fill={file.favorite ? "currentColor" : "none"} /></button>
    </div>
    <div className="file-card-copy">
      <div className="file-card-title-row">
        <div className="file-name-wrap"><strong title={fileName(file)}>{file.name}</strong><span>{kindLabel(file.kind)}</span></div>
        <div className="file-actions">
          {!file.trashed && canPreview(file.kind) && <button className="ghost-icon" onClick={onPreview} title="Vista previa">{file.kind === "audio" || file.kind === "video" ? <Play size={16} /> : <Eye size={16} />}</button>}
          {!file.trashed && <button className="ghost-icon" onClick={onDownload} title="Descargar"><ArrowDownToLine size={17} /></button>}
          {!file.trashed && <button className="ghost-icon" onClick={onMove} title="Mover a carpeta"><Move size={16} /></button>}
          <button className="ghost-icon" onClick={onTrash} title={file.trashed ? "Restaurar" : "Mover a Papelera"}>{file.trashed ? <RotateCcw size={16} /> : <Trash2 size={16} />}</button>
          {file.trashed && <button className="ghost-icon danger" onClick={onDelete} title="Eliminar definitivamente"><Trash2 size={16} /></button>}
        </div>
      </div>
      <div className="file-meta"><span>{formatBytes(file.sizeBytes)}</span><span className="meta-dot" /><span>{relativeDate(file.updatedAt)}</span></div>
    </div>
  </article>;
});

const FileRow = memo(function FileRow({ file, selected, isDragging, onSelect, onFavorite, onDownload, onPreview, onMove, onTrash, onDelete, onDragStart, onDragEnd, onPointerDown, onPointerMove, onPointerUp, onPointerCancel }: FileActions) {
  const Icon = kindIcon(file.kind);
  return <article className={`file-row ${selected ? "selected" : ""} ${isDragging ? "is-touch-dragging" : ""}`} draggable={false} tabIndex={0} aria-label={fileName(file)} onClick={(event) => { if (!(event.target as Element).closest("button,input,label,select,a")) onSelect(); }} onKeyDown={(event) => { if (event.target === event.currentTarget && event.key === " ") { event.preventDefault(); onSelect(); } }} onDragStart={onDragStart} onDragEnd={onDragEnd} onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={onPointerCancel}>
    <div className="file-row-name">
      <input type="checkbox" checked={selected} onChange={onSelect} aria-label={`Seleccionar ${file.name}`} />
      <div className={`small-file-icon kind-${file.kind}`}><FileThumbnail file={file}><Icon size={18} /></FileThumbnail></div>
      <div><strong title={fileName(file)}>{fileName(file)}</strong><small>{kindLabel(file.kind)}</small></div>
    </div>
    <span>{file.folder}</span><span>{formatBytes(file.sizeBytes)}</span><span>{relativeDate(file.updatedAt)}</span>
    <div className="row-actions">
      <button className={`ghost-icon ${file.favorite ? "active" : ""}`} onClick={onFavorite} aria-label="Favorito" aria-pressed={file.favorite}><Star size={16} fill={file.favorite ? "currentColor" : "none"} /></button>
      {!file.trashed && canPreview(file.kind) && <button className="ghost-icon" onClick={onPreview} title="Vista previa">{file.kind === "audio" || file.kind === "video" ? <Play size={16} /> : <Eye size={16} />}</button>}
      {!file.trashed && <button className="ghost-icon" onClick={onDownload} title="Descargar"><ArrowDownToLine size={16} /></button>}
      {!file.trashed && <button className="ghost-icon" onClick={onMove} title="Mover a carpeta"><Move size={15} /></button>}
      <button className="ghost-icon" onClick={onTrash} title={file.trashed ? "Restaurar" : "Mover a Papelera"}>{file.trashed ? <RotateCcw size={16} /> : <Trash2 size={16} />}</button>
      {file.trashed && <button className="ghost-icon danger" onClick={onDelete} title="Eliminar definitivamente"><Trash2 size={16} /></button>}
    </div>
  </article>;
});

export default App;
