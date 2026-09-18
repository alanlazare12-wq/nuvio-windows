import { useState, type Dispatch, type SetStateAction } from "react";
import { createFolder } from "./bridge/files";
import { readableError } from "./bridge/shared";
import {
  inspectDroppedPaths,
  prepareUploadBatch,
  prepareUploadItemsBatch,
  prepareZipUploads,
  scanDirectoryForUpload,
  selectFilesForUpload,
  selectFolderForUpload,
} from "./bridge/uploads";
import type {
  CloudFolder,
  DashboardData,
  DirectoryUploadPlan,
  SectionKey,
  SkippedUploadItem,
  UploadPreparationDecision,
} from "./types";
import { useUploadPreparation } from "./useUploadPreparation";

type UseUploadActionsOptions = {
  dashboard: DashboardData | null;
  section: SectionKey;
  currentFolderId: string | null;
  setSection: Dispatch<SetStateAction<SectionKey>>;
  setCurrentFolderId: Dispatch<SetStateAction<string | null>>;
  setMobileMenu: Dispatch<SetStateAction<boolean>>;
  onRequireConnection: () => void;
  refreshDashboard: () => Promise<DashboardData | undefined>;
  setNotice: Dispatch<SetStateAction<string | null>>;
};

type ZipProgress = {
  processed: number;
  total: number;
  name: string;
};

type PreparedFailure = {
  ok: false;
  path: string;
  error: string;
};

export function useUploadActions({
  dashboard,
  section,
  currentFolderId,
  setSection,
  setCurrentFolderId,
  setMobileMenu,
  onRequireConnection,
  refreshDashboard,
  setNotice,
}: UseUploadActionsOptions) {
  const [uploadBusy, setUploadBusy] = useState(false);
  const [zipBeforeUpload, setZipBeforeUpload] = useState(false);
  const [zipProgress, setZipProgress] = useState<ZipProgress | null>(null);
  const [skippedFiles, setSkippedFiles] = useState<SkippedUploadItem[]>([]);
  const [showSkippedModal, setShowSkippedModal] = useState(false);

  const {
    uploadAdvisory,
    resolveUploadPreparation,
    chooseUploadPreparation: finishUploadDecision,
  } = useUploadPreparation(zipBeforeUpload);

  const zipUploads = async (
    items: { path: string; name?: string }[],
    folderId?: string | null,
  ) => {
    setZipProgress({ processed: 0, total: 0, name: "Preparando ZIP…" });
    setNotice("Creando y verificando volúmenes ZIP de hasta 2 GB…");
    try {
      return await prepareZipUploads(
        items,
        folderId,
        (processed, total, name) => setZipProgress({ processed, total, name }),
      );
    } finally {
      setZipProgress(null);
    }
  };

  const prepareSelectedUploads = (
    paths: string[],
    concurrency: number,
    onSettled: Parameters<typeof prepareUploadBatch>[2],
    folderId: string | null | undefined,
    decision: Exclude<UploadPreparationDecision, "cancel">,
  ) => decision === "archive"
    ? zipUploads(paths.map((path) => ({ path })), folderId)
    : prepareUploadBatch(paths, concurrency, onSettled, folderId);

  const failuresToSkipped = (
    failed: PreparedFailure[],
    plan?: DirectoryUploadPlan,
  ): SkippedUploadItem[] => failed.map((item) => {
    const scanned = plan?.files.find((file) => file.absolutePath === item.path);
    return {
      fileName: scanned?.relativePath ?? item.path.split(/[\\/]/).pop() ?? item.path,
      path: item.path,
      reason: item.error,
      ...(scanned ? { sizeBytes: scanned.size } : {}),
    };
  });

  const publishFailures = (
    failed: PreparedFailure[],
    mode: "replace" | "append",
    plan?: DirectoryUploadPlan,
  ) => {
    if (!failed.length) return;
    const skipped = failuresToSkipped(failed, plan);
    setSkippedFiles((current) => mode === "replace" ? skipped : [...current, ...skipped]);
    setShowSkippedModal(true);
  };

  const summarizePrepared = (
    results: Awaited<ReturnType<typeof prepareUploadBatch>>,
  ) => {
    const queued = results.filter((result) => result.ok && !result.upload.duplicate).length;
    const duplicates = results.filter((result) => result.ok && result.upload.duplicate).length;
    const failed = results.filter((result): result is PreparedFailure => !result.ok);
    const parts = [
      queued ? `${queued} listo${queued === 1 ? "" : "s"} para subir` : null,
      duplicates ? `${duplicates} duplicado${duplicates === 1 ? "" : "s"}` : null,
      failed.length
        ? `${failed.length} omitido${failed.length === 1 ? "" : "s"} (límite Telegram)`
        : null,
    ].filter(Boolean);
    return { queued, duplicates, failed, parts };
  };

  const handleUpload = async () => {
    if (uploadBusy) return;
    if (!dashboard?.telegramConnected) {
      onRequireConnection();
      return;
    }

    setUploadBusy(true);
    setNotice(null);
    try {
      const selected = await selectFilesForUpload();
      if (!selected.length) return;

      const decision = await resolveUploadPreparation(
        selected.map((path) => ({ path })),
      );
      if (decision === "cancel") {
        setNotice("Subida cancelada. No se modificó ningún archivo.");
        return;
      }

      const targetFolderId = section === "files" ? currentFolderId : null;
      if (section !== "files") setCurrentFolderId(null);
      setSection("files");
      setMobileMenu(false);

      const results = await prepareSelectedUploads(
        selected,
        dashboard.settings.preparationConcurrency,
        (_result, completed, total) => {
          setNotice(`Preparando archivos ${completed}/${total}…`);
        },
        targetFolderId,
        decision,
      );

      const { failed, parts } = summarizePrepared(results);
      publishFailures(failed, "replace");
      setNotice(parts.join(" · ") || "No se añadieron archivos");
      await refreshDashboard();
    } catch (error) {
      setNotice(`No se pudo preparar la selección: ${readableError(error)}`);
    } finally {
      setUploadBusy(false);
    }
  };

  const executeUploadFolderPlan = async (
    plan: DirectoryUploadPlan,
    targetFolderId: string | null,
    decision: Exclude<UploadPreparationDecision, "cancel">,
  ): Promise<void> => {
    if (!plan.files.length && !plan.folders.length) {
      setNotice(`La carpeta "${plan.rootName}" está vacía.`);
      return;
    }

    if (decision === "archive" && plan.files.length > 0) {
      const results = await zipUploads(
        plan.files.map((file) => ({
          path: file.absolutePath,
          name: `${plan.rootName}/${file.relativePath}`,
        })),
        targetFolderId,
      );
      const queued = results.filter(
        (result) => result.ok && !result.upload.duplicate,
      ).length;
      const duplicates = results.filter(
        (result) => result.ok && result.upload.duplicate,
      ).length;
      const errors = results.filter((result) => !result.ok);
      setNotice(
        `${queued} ZIP listos · ${duplicates} duplicados${errors.length
          ? ` · ${errors.map((result) => result.ok ? "" : result.error).join("; ")}`
          : ""}`,
      );
      return;
    }

    setNotice(`Creando estructura de carpeta "${plan.rootName}"…`);
    const knownFolders: CloudFolder[] = [...(dashboard?.folders ?? [])];
    const folderMap = new Map<string, string>();

    const existingRoot = knownFolders.find(
      (folder) =>
        !folder.trashed
        && folder.parentId === targetFolderId
        && folder.name.toLowerCase() === plan.rootName.toLowerCase(),
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
        (folder) =>
          !folder.trashed
          && folder.parentId === parentFolderId
          && folder.name.toLowerCase() === folderName.toLowerCase(),
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
      return {
        path: file.absolutePath,
        folderId: folderMap.get(fileFolderRel) ?? rootId,
      };
    });

    if (!itemsToPrepare.length) {
      setNotice(
        `Carpeta "${plan.rootName}" creada con ${plan.folders.length} subcarpeta${plan.folders.length === 1 ? "" : "s"}`,
      );
      return;
    }

    const concurrency = dashboard?.settings.preparationConcurrency || 4;
    const results = await prepareUploadItemsBatch(
      itemsToPrepare,
      concurrency,
      (_result, completed, total) => {
        setNotice(`Preparando "${plan.rootName}" (${completed}/${total})…`);
      },
    );

    const { failed, parts } = summarizePrepared(results);
    publishFailures(failed, "append", plan);
    setNotice(
      `Carpeta "${plan.rootName}": ${parts.join(" · ") || "Estructura creada"}`,
    );
  };

  const handleUploadFolder = async () => {
    if (uploadBusy) return;
    if (!dashboard?.telegramConnected) {
      onRequireConnection();
      return;
    }

    setUploadBusy(true);
    setNotice(null);
    try {
      const plan = await selectFolderForUpload();
      if (!plan) return;

      const decision: UploadPreparationDecision = plan.files.length
        ? await resolveUploadPreparation(
            plan.files.map((file) => ({
              path: file.absolutePath,
              name: `${plan.rootName}/${file.relativePath}`,
              sizeBytes: file.size,
            })),
          )
        : "direct";

      if (decision === "cancel") {
        setNotice("Subida de carpeta cancelada. No se modificó ningún archivo.");
        return;
      }

      const targetFolderId = section === "files" ? currentFolderId : null;
      if (section !== "files") setCurrentFolderId(null);
      setSection("files");
      setMobileMenu(false);

      await executeUploadFolderPlan(plan, targetFolderId, decision);
      await refreshDashboard();
    } catch (error) {
      setNotice(`No se pudo subir la carpeta: ${readableError(error)}`);
    } finally {
      setUploadBusy(false);
    }
  };

  const handleExternalDrop = async (
    paths: string[],
    targetFolderId: string | null,
  ) => {
    if (uploadBusy) return;
    if (!dashboard?.telegramConnected) {
      setNotice("Conecta Telegram para subir archivos a Nuvio.");
      onRequireConnection();
      return;
    }

    setUploadBusy(true);
    setNotice("Analizando elementos arrastrados…");
    try {
      const inspected = await inspectDroppedPaths(paths);
      const directories = inspected.filter((item) => item.isDir);
      const files = inspected.filter((item) => !item.isDir);

      if (section !== "files") {
        setCurrentFolderId(targetFolderId);
        setSection("files");
      }

      for (const dir of directories) {
        setNotice(`Escaneando carpeta "${dir.name}"…`);
        const plan = await scanDirectoryForUpload(dir.path);
        const decision: UploadPreparationDecision = plan.files.length
          ? await resolveUploadPreparation(
              plan.files.map((file) => ({
                path: file.absolutePath,
                name: `${plan.rootName}/${file.relativePath}`,
                sizeBytes: file.size,
              })),
            )
          : "direct";

        if (decision === "cancel") {
          setNotice("Arrastre cancelado. No se modificó ningún archivo.");
          return;
        }
        await executeUploadFolderPlan(plan, targetFolderId, decision);
      }

      if (files.length > 0) {
        const decision = await resolveUploadPreparation(
          files.map((file) => ({ path: file.path, name: file.name })),
        );
        if (decision === "cancel") {
          setNotice("Arrastre cancelado. No se modificó ningún archivo.");
          return;
        }

        setNotice(
          `Preparando ${files.length} archivo${files.length === 1 ? "" : "s"}…`,
        );
        const results = await prepareSelectedUploads(
          files.map((file) => file.path),
          dashboard.settings.preparationConcurrency || 4,
          (_result, completed, total) => {
            setNotice(`Preparando archivos (${completed}/${total})…`);
          },
          targetFolderId,
          decision,
        );

        const { failed, parts } = summarizePrepared(results);
        publishFailures(failed, "append");
        if (!directories.length) {
          setNotice(`Archivos: ${parts.join(" · ") || "Listo"}`);
        }
      }

      await refreshDashboard();
    } catch (error) {
      setNotice(`Error al procesar arrastre: ${readableError(error)}`);
    } finally {
      setUploadBusy(false);
    }
  };

  return {
    uploadBusy,
    zipBeforeUpload,
    setZipBeforeUpload,
    zipProgress,
    skippedFiles,
    clearSkippedFiles: () => setSkippedFiles([]),
    showSkippedModal,
    setShowSkippedModal,
    uploadAdvisory,
    finishUploadDecision,
    handleUpload,
    handleUploadFolder,
    handleExternalDrop,
  };
}
