export type FileKind =
  | "image"
  | "video"
  | "audio"
  | "pdf"
  | "document"
  | "spreadsheet"
  | "presentation"
  | "text"
  | "code"
  | "archive"
  | "ebook"
  | "database"
  | "font"
  | "package"
  | "model"
  | "other";

export type CloudFile = {
  id: string;
  name: string;
  extension: string;
  kind: FileKind;
  sizeBytes: number;
  updatedAt: string;
  favorite: boolean;
  trashed: boolean;
  folder: string;
  folderId?: string | null;
  tags: string[];
  provider: "telegram" | "local";
  telegramMessageId?: string | null;
};

export type CatalogPage = {
  files: CloudFile[];
  total: number;
  offset: number;
  limit: number;
  hasMore: boolean;
};

export type CloudFolder = {
  id: string;
  name: string;
  parentId?: string | null;
  trashed: boolean;
  createdAt: number;
  updatedAt: number;
  fileCount: number;
  childCount: number;
  sizeBytes: number;
};

export type TransferStatus =
  | "waiting"
  | "analyzing"
  | "copying"
  | "ready"
  | "queued"
  | "uploading"
  | "downloading"
  | "confirming"
  | "retry_wait"
  | "running"
  | "paused"
  | "completed"
  | "failed"
  | "duplicate"
  | "cancelled";

export type TransferPhase = TransferStatus | "error";

export type TransferJob = {
  id: string;
  fileName: string;
  direction: "upload" | "download";
  progress: number;
  status: TransferStatus;
  phase: TransferPhase;
  speedLabel: string;
  processedBytes: number;
  totalBytes: number;
  speedBps: number;
  etaSeconds?: number | null;
  attempts: number;
  maxAttempts: number;
  error?: string | null;
  canPause: boolean;
  canRetry: boolean;
  canCancel: boolean;
  sourceDeleteAvailable?: boolean;
  sourceDeleted?: boolean;
  sourceDeleteError?: string | null;
  startedAt?: number | null;
  updatedAt: number;
};

export type QueueSummary = {
  total: number;
  completed: number;
  pending: number;
  failed: number;
  active: number;
  processedBytes: number;
  totalBytes: number;
  speedBps: number;
  etaSeconds?: number | null;
  cacheBytes: number;
  cacheLimitBytes: number;
};

export type AppSettings = {
  preparationConcurrency: number;
  uploadConcurrency: number;
  downloadConcurrency: number;
  cacheLimitBytes: number;
  rememberSession: boolean;
  conflictPolicy: "skip" | "rename";
  deleteOriginalAfterUpload: boolean;
  speedLimitBps?: number | null;
  resourceProfile: "low" | "balanced" | "max";
};

export type SyncPhase =
  | "idle"
  | "starting"
  | "scanning"
  | "folders"
  | "files"
  | "applying"
  | "publishing"
  | "cancelled"
  | "complete"
  | "error";

export type SyncProgress = {
  active: boolean;
  phase: SyncPhase;
  scanned: number;
  total?: number | null;
  percent?: number | null;
  etaSeconds?: number | null;
  detail?: string | null;
  error?: string | null;
};
export type SyncDelta = {
  cursor: number;
  syncProgress: SyncProgress;
  files: CloudFile[];
  removedIds: string[];
  folders?: CloudFolder[] | null;
};
export type UploadedImageCleanupSummary = { count: number; bytes: number };
export type UploadedImageCleanupResult = { deleted: number; releasedBytes: number; skipped: number; failed: number };
export type DashboardStatus = {
  syncProgress?: SyncProgress;
  transfers: TransferJob[];
  transferHistory?: TransferJob[] | null;
  totalBytes: number;
  fileCount: number;
  favoriteCount: number;
  trashCount: number;
  recentCount: number;
  telegramConnected: boolean;
  telegramAccountLabel?: string | null;
  providerStatus: string;
  queueSummary: QueueSummary;
  settings: AppSettings;
  isPremium?: boolean;
  catalogCursor: number;
  historyCursor: number;
};
export type DashboardData = Omit<DashboardStatus, "transferHistory"> & {
  files: CloudFile[];
  folders: CloudFolder[];
  transferHistory: TransferJob[];
};

export type TelegramAuthStage =
  | "needsCredentials"
  | "initializing"
  | "phone"
  | "email"
  | "emailCode"
  | "code"
  | "password"
  | "qr"
  | "registration"
  | "ready"
  | "loggingOut"
  | "closing"
  | "closed";

export type TelegramEmailResetSnapshot = {
  state: "available" | "pending";
  seconds: number;
};

export type TelegramLoginEmailCodeInfo = {
  emailPattern: string;
  codeLength: number;
};

export type TelegramLoginEmailStatus = {
  available: boolean;
  required: boolean;
  emailPattern?: string | null;
};

export type TelegramIdentityProvider = "google" | "apple";

export type TelegramAuthSnapshot = {
  stage: TelegramAuthStage;
  message: string;
  connected: boolean;
  accountLabel?: string | null;
  hint?: string | null;
  qrLink?: string | null;
  qrSvg?: string | null;
  isPremium: boolean;
  timeout?: number | null;
  codeType?: string | null;
  nextCodeType?: string | null;
  codeLength?: number | null;
  fragmentUrl?: string | null;
  emailPattern?: string | null;
  emailCodeLength?: number | null;
  allowGoogleId: boolean;
  allowAppleId: boolean;
  emailReset?: TelegramEmailResetSnapshot | null;
  futureAuthTokenCount: number;
};

export type PreparedUpload = {
  transferId: string;
  fileName: string;
  localPath: string;
  sizeBytes: number;
  sha256: string;
  duplicate: boolean;
  encrypted: boolean;
  status: TransferStatus;
};

export type ScannedUploadFile = {
  relativePath: string;
  absolutePath: string;
  size: number;
};

export type DirectoryUploadPlan = {
  rootName: string;
  folders: string[];
  files: ScannedUploadFile[];
  totalBytes: number;
};

export type BatchDownloadItem = {
  fileId: string;
  transferId?: string | null;
  status: "queued" | "skipped" | "error";
  path?: string | null;
  message: string;
};

export type MediaReady = {
  fileId: string;
  path: string;
  sizeBytes: number;
  fromCache: boolean;
};

export type FileFilter = "all" | FileKind;
export type SectionKey = "home" | "files" | "favorites" | "recent" | "history" | "trash";
export type SortKey = "recent" | "oldest" | "name" | "size";
export type TransferFilter = "all" | "pending" | "failed" | "completed";

export type SkippedUploadItem = {
  fileName: string;
  path: string;
  reason: string;
  sizeBytes?: number;
};

export type DroppedPathInfo = {
  path: string;
  isDir: boolean;
  name: string;
};

export type UploadAdvisoryInput = {
  path: string;
  name?: string;
  sizeBytes?: number;
};

export type UploadRecommendation = "direct" | "compress" | "splitVolumes";
export type UploadPreparationDecision = "direct" | "archive" | "cancel";

export type UploadAdvisoryFile = {
  name: string;
  sizeBytes: number;
  exceedsTelegramLimit: boolean;
};

export type UploadAdvisory = {
  recommendation: UploadRecommendation;
  shouldPrompt: boolean;
  fileCount: number;
  imageCount: number;
  totalBytes: number;
  unknownSizeCount: number;
  oversizedCount: number;
  telegramOversizedCount: number;
  telegramLimitBytes: number;
  largestFileBytes: number;
  oversizedFiles: UploadAdvisoryFile[];
  reasonCodes: string[];
};
