import {
  AlertCircle,
  AlertTriangle,
  Archive,
  ArrowDownToLine,
  CheckCircle2,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Clock3,
  Cloud,
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
import {
  backgroundApp,
  listenMobileBack,
  loadDashboard,
  pauseQueue,
  readableError,
  resumeQueue,
  setFavorite,
  setTrashed,
  updateSetting,
} from "./bridge";
import type {
  CloudFile,
  CloudFolder,
  DashboardData,
  FileFilter,
  FileKind,
  SectionKey,
  SortKey,
  TransferFilter,
} from "./types";
import "./App.css";
import { Dialog } from "./Dialog";
import { FileThumbnail, clearThumbnailCache } from "./FileThumbnail";
import { formatBytes, formatEta, formatSpeed } from "./format";
import { PdfPreview } from "./components/PdfPreview";
import { SkippedUploadsModal } from "./components/SkippedUploadsModal";
import { SyncProgressPanel } from "./components/SyncProgressPanel";
import { TransferHistoryRow, TransferRow } from "./components/TransferRows";
import { FolderEditorDialog } from "./components/FolderEditorDialog";
import { MoveToFolderDialog } from "./components/MoveToFolderDialog";
import { TelegramConnectModal } from "./components/TelegramConnectModal";
import { UploadPreparationDialog } from "./components/UploadPreparationDialog";
import { useCatalogSync } from "./useCatalogSync";
import { useDashboardLifecycle } from "./useDashboardLifecycle";
import { useFileDragDrop } from "./useFileDragDrop";
import { useFolderActions } from "./useFolderActions";
import { useMaintenanceActions } from "./useMaintenanceActions";
import { useNativeFileDrop } from "./useNativeFileDrop";
import { canPreview, useMediaPreview } from "./useMediaPreview";
import { useTransfers } from "./useTransfers";
import { useTrashActions } from "./useTrashActions";
import { useUploadActions } from "./useUploadActions";

export { detectCountryCallingCode } from "./components/TelegramConnectModal";

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

function App() {
  const [dashboard, setDashboard] = useState<DashboardData | null>(null);
  const [section, setSection] = useState<SectionKey>("home");
  const [currentFolderId, setCurrentFolderId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [fileFilter, setFileFilter] = useState<FileFilter>("all");
  const [sort, setSort] = useState<SortKey>("recent");
  const [view, setView] = useState<"grid" | "list">("grid");
  const [selectedTag, setSelectedTag] = useState<string | null>(null);
  const [filterPanel, setFilterPanel] = useState(false);
  const [connectModal, setConnectModal] = useState(false);
  const [mobileMenu, setMobileMenu] = useState(false);
  const [darkMode, setDarkMode] = useState(initialDarkMode);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [appNotice, setAppNotice] = useState<string | null>(null);
  const [selectedFiles, setSelectedFiles] = useState<Set<string>>(() => new Set());
  useEffect(() => { if (!dashboard?.telegramConnected) clearThumbnailCache(); }, [dashboard?.telegramConnected]);
  const hasAutoPromptedLoginRef = useRef(false);
  useEffect(() => {
    if (!dashboard) return;
    if (!hasAutoPromptedLoginRef.current) {
      hasAutoPromptedLoginRef.current = true;
      if (!dashboard.telegramConnected) {
        setConnectModal(true);
      }
    }
  }, [dashboard?.telegramConnected]);
  const filterTabsRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const fileSearchInputRef = useRef<HTMLInputElement>(null);
  const dashboardRequestRef = useRef(0);

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

  const {
    draggingFileIds,
    draggingFolderId,
    dragTarget,
    setDragTarget,
    touchDragPosition,
    dragActiveRef,
    clearFileDrag,
    desktopDragStart,
    desktopDrop,
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
  } = useFileDragDrop({
    dashboard,
    currentFolderId,
    selectedFiles,
    setSelectedFiles,
    setCurrentFolderId,
    setSection,
    refreshDashboard,
    setNotice: setAppNotice,
    onRequireConnection: () => setConnectModal(true),
  });

  const {
    uploadBusy,
    zipBeforeUpload,
    setZipBeforeUpload,
    zipProgress,
    skippedFiles,
    clearSkippedFiles,
    showSkippedModal,
    setShowSkippedModal,
    uploadAdvisory,
    finishUploadDecision,
    handleUpload,
    handleUploadFolder,
    handleExternalDrop,
  } = useUploadActions({
    dashboard,
    section,
    currentFolderId,
    setSection,
    setCurrentFolderId,
    setMobileMenu,
    onRequireConnection: () => setConnectModal(true),
    refreshDashboard,
    setNotice: setAppNotice,
  });

  const {
    externalDragActive,
    externalDragTargetName,
  } = useNativeFileDrop({
    dashboard,
    section,
    currentFolderId,
    handleExternalDrop,
  });

  const {
    cleanupBusy,
    handleExportDiagnostics,
    handleFreeUploadedImages,
  } = useMaintenanceActions({
    refreshDashboard,
    setNotice: setAppNotice,
  });

  const {
    folderEditor,
    setFolderEditor,
    moveDialog,
    setMoveDialog,
    handleSaveFolder,
    handleMoveConfirm,
    handleDeleteFolder,
  } = useFolderActions({
    currentFolderId,
    refreshDashboard,
    setSelectedFiles,
    setNotice: setAppNotice,
  });

  const {
    syncBusy,
    syncDismissed,
    setSyncDismissed,
    isSyncing,
    manualSyncPollingRef,
    handleSync,
  } = useCatalogSync({
    dashboard,
    setDashboard,
    refreshDashboard,
    setNotice: setAppNotice,
  });

  const {
    mediaFile,
    mediaSrc,
    previewText,
    mediaBusy,
    mediaError,
    setMediaError,
    closeMedia,
    handleMedia,
    handleClearCache,
  } = useMediaPreview({
    connected: Boolean(dashboard?.telegramConnected),
    onRequireConnection: () => setConnectModal(true),
    refreshDashboard,
    setNotice: setAppNotice,
    formatBytes,
  });

  const {
    handleBulkTrash,
    handlePermanentDelete,
    handleEmptyTrash,
  } = useTrashActions({
    dashboard,
    mediaFileId: mediaFile?.id ?? null,
    closeMedia,
    setSelectedFiles,
    refreshDashboard,
    setNotice: setAppNotice,
    onRequireConnection: () => setConnectModal(true),
  });

  const {
    transferFilter,
    setTransferFilter,
    filteredTransfers,
    handleBulkDownload,
    handleClearHistory,
    handleDeleteVerifiedSource,
  } = useTransfers({
    dashboard,
    selectedFiles,
    clearSelection: () => setSelectedFiles(new Set()),
    refreshDashboard,
    setNotice: setAppNotice,
    onRequireConnection: () => {
      setMobileMenu(false);
      setConnectModal(true);
    },
  });

  useDashboardLifecycle({
    dashboard,
    refreshDashboard,
    dragActiveRef,
    manualSyncPollingRef,
    invalidateDashboardRequests: () => {
      dashboardRequestRef.current++;
    },
    setNotice: setAppNotice,
  });

  useEffect(() => {
    document.documentElement.dataset.theme = darkMode ? "dark" : "light";
    try { localStorage.setItem("nuvio-theme", darkMode ? "dark" : "light"); } catch { /* The theme still works for this session. */ }
  }, [darkMode]);

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
    if (!dashboard) return [] as CloudFolder[];
    const needle = query.trim().toLowerCase();
    if (section === "home") {
      if (!needle) return [] as CloudFolder[];
      return dashboard.folders
        .filter((folder) => !folder.trashed && folder.name.toLowerCase().includes(needle))
        .sort((a, b) => a.name.localeCompare(b.name, "es", { numeric: true, sensitivity: "base" }));
    }
    if (section !== "files") return [] as CloudFolder[];
    return dashboard.folders
      .filter((folder) => !folder.trashed && (needle ? true : (folder.parentId ?? null) === currentFolderId))
      .filter((folder) => !needle || folder.name.toLowerCase().includes(needle))
      .sort((a, b) => a.name.localeCompare(b.name, "es", { numeric: true, sensitivity: "base" }));
  }, [dashboard, currentFolderId, query, section]);

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
    return section === "home" ? (needle ? output : output.slice(0, 12)) : output;
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

  const action = async (operation: () => Promise<unknown>) => {
    try {
      await operation();
      await refreshDashboard();
    } catch (error) {
      setAppNotice(readableError(error));
    }
  };

  const scrollFileTypes = (direction: -1 | 1) => {
    filterTabsRef.current?.scrollBy({ left: direction * 280, behavior: "smooth" });
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
    <div
      className="app-shell"
      onDragOverCapture={allowInternalDragCursor}
      onDrop={cancelInternalDesktopDrop}
    >
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
              <button className={`secondary-button sync-action-button ${isSyncing ? "is-syncing" : ""}`} disabled={isSyncing || !dashboard.telegramConnected} onClick={() => void handleSync()} title={isSyncing ? "Sincronización en curso con Telegram…" : "Sincronizar con Telegram"} aria-label={isSyncing ? "Sincronizando…" : "Sincronizar"}><RefreshCw size={17} className={isSyncing ? "spin-icon" : ""} /><span className="sync-button-label">{isSyncing ? "Sincronizando…" : "Sincronizar"}</span></button>
              <button className="primary-button" onClick={() => void handleUpload()} disabled={uploadBusy}><Upload size={17} /> {uploadBusy ? "Preparando…" : "Subir"}</button>
            </div>
          </section>

          <div className="zip-upload-options">
            <label><input type="checkbox" checked={zipBeforeUpload} disabled={uploadBusy} onChange={event => setZipBeforeUpload(event.target.checked)} /> Comprimir antes de subir o dividir automáticamente (sin preguntar)</label>
            <small>
              {zipBeforeUpload
                ? "Activado: Nuvio preparará la selección como ZIP y dividirá automáticamente cualquier archivo que supere 2 GB."
                : "Desactivado: Nuvio solo te sugerirá comprimir cuando haya muchos archivos/imágenes o dividir cuando detecte archivos mayores de 2 GB. Siempre podrás decir que no."}
            </small>
          </div>
          {zipProgress && <section className="sync-progress-panel" aria-label="Progreso de compresión">
            <div><strong>{zipProgress.processed === zipProgress.total && zipProgress.total > 0 ? "Preparando ZIP para subir…" : "Comprimiendo…"}</strong><span>{zipProgress.total > 0 ? `${Math.min(100, Math.floor(zipProgress.processed * 100 / zipProgress.total))}%` : "Calculando…"}</span></div>
            <progress aria-label="Compresión ZIP" max={zipProgress.total || 1} value={zipProgress.total ? zipProgress.processed : undefined} />
            <small>{zipProgress.name}</small>
          </section>}

          {!syncDismissed && (
            <SyncProgressPanel
              syncProgress={dashboard.syncProgress}
              syncBusy={syncBusy}
              onDismiss={() => setSyncDismissed(true)}
            />
          )}

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
                  onClick={clearSkippedFiles}
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
            {dashboard.transferHistory.length === 0 ? <div className="empty-state"><div className="empty-icon"><Clock3 size={25} /></div><h3>Aún no hay historial</h3><p>Las transferencias terminadas aparecerán aquí sin ocupar espacio en la cola activa.</p></div> : <div className="history-list">{dashboard.transferHistory.map((job) => <TransferHistoryRow key={job.id} job={job} onDeleteSource={() => void handleDeleteVerifiedSource(job)} />)}</div>}
          </section>}

          {section !== "history" && <section
            className={`files-section ${section === "files" && draggingFileIds.length > 0 && dragTarget === currentFolderDropKey ? "current-folder-drop-active" : ""}`}
            data-nuvio-drop-folder={section === "files" ? currentFolderDropKey : undefined}
            onDragEnter={section === "files" ? handleCurrentFolderDragOver : undefined}
            onDragOver={section === "files" ? handleCurrentFolderDragOver : undefined}
            onDrop={section === "files" ? handleCurrentFolderDrop : undefined}
          >
            <div className="section-toolbar-top">
              <div><h2>{section === "home" ? (query.trim() ? "Resultados de búsqueda" : "Archivos recientes") : activeSection}</h2><span>{files.length + (section === "files" || (section === "home" && Boolean(query.trim())) ? visibleFolders.length : 0)} elementos</span></div>
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
              {section !== "trash" && <div className="source-space-actions">
                <label className="delete-source-control" title="Nuvio verifica tamaño y SHA-256 otra vez antes de borrar el original">
                  <input type="checkbox" checked={dashboard.settings.deleteOriginalAfterUpload} onChange={(event) => void action(() => updateSetting("delete_original_after_upload", event.target.checked ? "1" : "0"))} />
                  <span><strong>Liberar espacio tras subir</strong><small>Borrar original solo después de verificarlo</small></span>
                </label>
                <button
                  className="secondary-button compact source-cleanup-button"
                  type="button"
                  disabled={cleanupBusy}
                  onClick={() => void handleFreeUploadedImages()}
                  title="Busca originales locales registrados por Nuvio y los elimina solo después de verificar tamaño y SHA-256"
                >
                  <HardDrive size={14} /> {cleanupBusy ? "Liberando…" : "Liberar imágenes subidas"}
                </button>
              </div>}
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

            {(section === "files" || (section === "home" && Boolean(query.trim()))) && visibleFolders.length > 0 && <div className="folder-grid">{visibleFolders.map((folder) => <FolderCard
              key={folder.id}
              folder={folder}
              dropActive={dragTarget === folder.id}
              isDragging={draggingFolderId === folder.id}
              onOpen={() => { setCurrentFolderId(folder.id); setSelectedFiles(new Set()); setSection("files"); }}
              onRename={() => setFolderEditor({ mode: "rename", folder })}
              onMove={() => setMoveDialog({ kind: "folder", folder })}
              onDelete={() => void handleDeleteFolder(folder)}
              onPointerDown={(event) => folderTouchDragStart(event, folder)}
              onPointerMove={touchDragMove}
              onPointerUp={touchDragEnd}
              onPointerCancel={clearFileDrag}
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
                <div className="file-grid">{visibleFiles.map((file) => <FileCard key={file.id} file={file} selected={selectedFiles.has(file.id)} isDragging={draggingFileIds.includes(file.id)} onSelect={() => toggleSelection(file.id)} onFavorite={() => void action(() => setFavorite(file.id, !file.favorite))} onDownload={() => void handleBulkDownload([file.id])} onPreview={() => void handleMedia(file)} onMove={() => setMoveDialog({ kind: "files", ids: [file.id] })} onTrash={() => void action(() => setTrashed(file.id, !file.trashed))} onDelete={() => void handlePermanentDelete([file.id])} onDragStart={(event) => desktopDragStart(event, file)} onDragEnd={clearFileDrag} onPointerDown={(event) => touchDragStart(event, file)} onPointerMove={touchDragMove} onPointerUp={touchDragEnd} onPointerCancel={clearFileDrag} />)}</div>
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
                <div className="file-list"><div className="file-list-head"><span>Nombre</span><span>Ubicación</span><span>Tamaño</span><span>Modificado</span><span /></div>{visibleFiles.map((file) => <FileRow key={file.id} file={file} selected={selectedFiles.has(file.id)} isDragging={draggingFileIds.includes(file.id)} onSelect={() => toggleSelection(file.id)} onFavorite={() => void action(() => setFavorite(file.id, !file.favorite))} onDownload={() => void handleBulkDownload([file.id])} onPreview={() => void handleMedia(file)} onMove={() => setMoveDialog({ kind: "files", ids: [file.id] })} onTrash={() => void action(() => setTrashed(file.id, !file.trashed))} onDelete={() => void handlePermanentDelete([file.id])} onDragStart={(event) => desktopDragStart(event, file)} onDragEnd={clearFileDrag} onPointerDown={(event) => touchDragStart(event, file)} onPointerMove={touchDragMove} onPointerUp={touchDragEnd} onPointerCancel={clearFileDrag} />)}</div>
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

      {touchDragPosition && (draggingFileIds.length > 0 || draggingFolderId) && (
        <div
          className={`touch-drag-badge ${dragTarget ? "has-target" : ""}`}
          style={{ left: touchDragPosition.x, top: touchDragPosition.y }}
          aria-hidden="true"
        >
          <Move size={16} />
          <strong>{draggingFolderId ? 1 : draggingFileIds.length}</strong>
          <span>
            {dragTarget
              ? dragTarget === "__root__"
                ? "Mover a Mi unidad"
                : `Mover a ${dashboard?.folders.find((f) => f.id === dragTarget)?.name ?? "carpeta"}`
              : draggingFolderId
              ? dashboard?.folders.find((folder) => folder.id === draggingFolderId)?.name ?? "carpeta"
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

      {uploadAdvisory && (
        <UploadPreparationDialog advisory={uploadAdvisory} onChoose={finishUploadDecision} />
      )}

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

function FolderCard({ folder, dropActive, isDragging, onOpen, onRename, onMove, onDelete, onPointerDown, onPointerMove, onPointerUp, onPointerCancel, onFileDragOver, onFileDragLeave, onFileDrop }: {
  folder: CloudFolder;
  dropActive: boolean;
  isDragging: boolean;
  onOpen: () => void;
  onRename: () => void;
  onMove: () => void;
  onDelete: () => void;
  onPointerDown: (event: React.PointerEvent<HTMLElement>) => void;
  onPointerMove: (event: React.PointerEvent<HTMLElement>) => void;
  onPointerUp: (event: React.PointerEvent<HTMLElement>) => void;
  onPointerCancel: (event: React.PointerEvent<HTMLElement>) => void;
  onFileDragOver: (event: React.DragEvent<HTMLElement>) => void;
  onFileDragLeave: (event: React.DragEvent<HTMLElement>) => void;
  onFileDrop: (event: React.DragEvent<HTMLElement>) => void;
}) {
  const empty = folder.fileCount === 0 && folder.childCount === 0;
  return (
    <article
      data-nuvio-drop-folder={folder.id}
      className={`folder-card ${dropActive ? "drag-over" : ""} ${isDragging ? "is-dragging" : ""}`}
      onDragOver={onFileDragOver}
      onDragEnter={onFileDragOver}
      onDragLeave={onFileDragLeave}
      onDrop={onFileDrop}
    >
      <div
        className="folder-open-area"
        role="button"
        tabIndex={0}
        onClick={onOpen}
        onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); onOpen(); } }}
        aria-label={`Abrir ${folder.name}`}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerCancel}
        onDragEnter={onFileDragOver}
        onDragOver={onFileDragOver}
        onDragLeave={onFileDragLeave}
        onDrop={onFileDrop}
      >
        <span className="folder-icon"><Folder size={23} fill="currentColor" /></span>
        <span className="folder-copy"><strong>{folder.name}</strong><small>{folder.fileCount} archivo{folder.fileCount === 1 ? "" : "s"} · {folder.childCount} carpeta{folder.childCount === 1 ? "" : "s"}</small></span>
        <ChevronRight size={16} />
      </div>
      <div className="folder-actions">
        <button className="ghost-icon" onClick={onRename} title="Renombrar"><Pencil size={15} /></button>
        <button className="ghost-icon" onClick={onMove} title="Mover carpeta"><Move size={15} /></button>
        <button className="ghost-icon danger" disabled={!empty} onClick={onDelete} title={empty ? "Eliminar carpeta" : "Vacía la carpeta antes de eliminarla"}><Trash2 size={15} /></button>
      </div>
    </article>
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
  return <article className={`file-card ${!file.trashed ? "is-draggable" : ""} ${selected ? "selected" : ""} ${isDragging ? "is-touch-dragging" : ""}`} draggable={false} tabIndex={0} aria-label={fileName(file)} onClick={(event) => { if (!(event.target as Element).closest("button,input,label,select,a")) onSelect(); }} onKeyDown={(event) => { if (event.target === event.currentTarget && event.key === " ") { event.preventDefault(); onSelect(); } }} onDragStart={onDragStart} onDragEnd={onDragEnd} onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={onPointerCancel}>
    <div className={`file-preview kind-${file.kind}`}>
      <label className="file-select-checkbox"><input type="checkbox" checked={selected} onChange={onSelect} aria-label={`Seleccionar ${file.name}`} /></label>
      <FileThumbnail file={file} onOpen={file.kind === "image" ? onPreview : undefined}><div className="file-type-icon"><Icon size={27} strokeWidth={1.7} /></div></FileThumbnail>
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
  return <article className={`file-row ${!file.trashed ? "is-draggable" : ""} ${selected ? "selected" : ""} ${isDragging ? "is-touch-dragging" : ""}`} draggable={false} tabIndex={0} aria-label={fileName(file)} onClick={(event) => { if (!(event.target as Element).closest("button,input,label,select,a")) onSelect(); }} onKeyDown={(event) => { if (event.target === event.currentTarget && event.key === " ") { event.preventDefault(); onSelect(); } }} onDragStart={onDragStart} onDragEnd={onDragEnd} onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={onPointerCancel}>
    <div className="file-row-name">
      <input type="checkbox" checked={selected} onChange={onSelect} aria-label={`Seleccionar ${file.name}`} />
      <div className={`small-file-icon kind-${file.kind}`}><FileThumbnail file={file} onOpen={file.kind === "image" ? onPreview : undefined}><Icon size={18} /></FileThumbnail></div>
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
