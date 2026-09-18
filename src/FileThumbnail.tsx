import { useEffect, useRef, useState, type ReactNode } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { loadPdfJs } from "./pdf";
import type { CloudFile } from "./types";

type Source = { kind: "image" | "pdf" | "text"; path: string | null; dataUrl: string | null; blurred?: boolean };
type Preview = { image?: string; text?: string; blurred?: boolean } | null;
type BatchSource = { id: string; source: Source | null };
type SourceWaiter = { resolve: (source: Source | null) => void; reject: (error: unknown) => void };
const cache = new Map<string, Preview>();
const pending = new Map<string, Promise<Preview>>();
const queue: Array<() => Promise<void>> = [];
let active = 0;
let generation = 0;

const sourceWaiters = new Map<string, SourceWaiter[]>();
let sourceBatchTimer: number | null = null;

function scheduleSourceBatch() {
  if (sourceBatchTimer != null) return;
  sourceBatchTimer = window.setTimeout(() => { void flushSourceBatch(); }, 0);
}

async function resolveIndividually(id: string): Promise<Source | null> {
  return invoke<Source | null>("prepare_thumbnail", { id });
}

async function flushSourceBatch() {
  sourceBatchTimer = null;
  const ids = [...sourceWaiters.keys()].slice(0, 32);
  if (!ids.length) return;

  const waiters = new Map<string, SourceWaiter[]>();
  for (const id of ids) {
    waiters.set(id, sourceWaiters.get(id) ?? []);
    sourceWaiters.delete(id);
  }

  let batch = new Map<string, Source | null>();
  try {
    const items = await invoke<BatchSource[]>("prepare_thumbnail_batch", { ids });
    batch = new Map(items.map((item) => [item.id, item.source]));
  } catch {
    // Older/partially upgraded backends still work through the individual command.
  }

  await Promise.all(ids.map(async (id) => {
    let source = batch.get(id) ?? null;
    if (!source) {
      try {
        source = await resolveIndividually(id);
      } catch (error) {
        for (const waiter of waiters.get(id) ?? []) waiter.reject(error);
        return;
      }
    }
    for (const waiter of waiters.get(id) ?? []) waiter.resolve(source);
  }));

  if (sourceWaiters.size) scheduleSourceBatch();
}

function requestThumbnailSource(id: string): Promise<Source | null> {
  return new Promise((resolve, reject) => {
    const waiters = sourceWaiters.get(id) ?? [];
    waiters.push({ resolve, reject });
    sourceWaiters.set(id, waiters);
    scheduleSourceBatch();
  });
}

export function clearThumbnailCache() {
  generation++;
  cache.clear();
  pending.clear();
  window.dispatchEvent(new Event("nuvio:thumbnails-cleared"));
}

function drain() {
  while (active < 8 && queue.length) {
    active++;
    void queue.shift()!().finally(() => { active--; drain(); });
  }
}

async function renderThumbnail(id: string): Promise<Preview> {
  const source = await requestThumbnailSource(id);
  if (!source) return null;
  const url = source.dataUrl ?? (source.path ? convertFileSrc(source.path) : null);
  if (!url) return null;
  // Telegram minithumbnails are already tiny progressive previews. Show them
  // directly and let CSS blur/upscale them instead of decoding and redrawing a
  // second bitmap first; this keeps large image libraries responsive.
  if (source.kind === "image" && source.blurred && source.dataUrl) {
    return { image: source.dataUrl, blurred: true };
  }
  if (source.kind === "text") {
    const response = await fetch(url);
    if (!response.ok) throw new Error("No se pudo leer la miniatura");
    return { text: (await response.text()).slice(0, 1600) };
  }
  const canvas = document.createElement("canvas");
  const context = canvas.getContext("2d");
  if (!context) return null;
  if (source.kind === "pdf") {
    const pdfjs = await loadPdfJs();
    const task = pdfjs.getDocument({ url });
    try {
      const pdf = await task.promise;
      const page = await pdf.getPage(1);
      const base = page.getViewport({ scale: 1 });
      const viewport = page.getViewport({ scale: Math.min(320 / base.width, 240 / base.height) });
      canvas.width = Math.max(1, Math.floor(viewport.width));
      canvas.height = Math.max(1, Math.floor(viewport.height));
      await page.render({ canvas, canvasContext: context, viewport }).promise;
      return { image: canvas.toDataURL("image/webp", 0.75), blurred: false };
    } finally { await task.destroy(); }
  }
  const response = await fetch(url);
  if (!response.ok) throw new Error("No se pudo leer la miniatura");
  const bitmap = await createImageBitmap(await response.blob(), { resizeWidth: 320, resizeQuality: "low" });
  try {
    const scale = Math.min(320 / bitmap.width, 240 / bitmap.height, 1);
    canvas.width = Math.max(1, Math.round(bitmap.width * scale));
    canvas.height = Math.max(1, Math.round(bitmap.height * scale));
    context.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
    return { image: canvas.toDataURL("image/webp", 0.75), blurred: Boolean(source.blurred) };
  } finally { bitmap.close(); }
}

function preload(key: string, id: string, isVisible: () => boolean, priority = false): Promise<Preview> {
  if (cache.has(key)) return Promise.resolve(cache.get(key)!);
  const existing = pending.get(key);
  if (existing) return existing;
  const epoch = generation;
  const result = new Promise<Preview>(resolve => {
    const task = async () => {
      if (epoch !== generation || !isVisible()) { pending.delete(key); resolve(null); return; }
      let preview: Preview = null;
      try { preview = await renderThumbnail(id); } catch { /* Keep the file type icon on failure. */ }
      if (epoch === generation) {
        cache.set(key, preview);
        if (cache.size > 256) cache.delete(cache.keys().next().value!);
        pending.delete(key);
        resolve(preview);
      } else { resolve(null); }
    };
    if (priority) queue.unshift(task); else queue.push(task);
  });
  pending.set(key, result);
  drain();
  return result;
}

const thumbnailListeners = new Map<Element, (intersecting: boolean) => void>();
let sharedObserver: IntersectionObserver | null = null;

function observeThumbnail(el: Element, cb: (intersecting: boolean) => void) {
  if (!sharedObserver) {
    sharedObserver = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          const listener = thumbnailListeners.get(entry.target);
          if (listener) listener(entry.isIntersecting);
        }
      },
      { rootMargin: "700px" }
    );
  }
  thumbnailListeners.set(el, cb);
  sharedObserver.observe(el);
  return () => {
    thumbnailListeners.delete(el);
    sharedObserver?.unobserve(el);
  };
}

export function FileThumbnail({ file, children, onOpen }: { file: CloudFile; children: ReactNode; onOpen?: () => void }) {
  const container = useRef<HTMLDivElement>(null);
  const [epoch, setEpoch] = useState(generation);
  const key = `${epoch}:${file.id}:${file.updatedAt}:${file.sizeBytes}`;
  const [preview, setPreview] = useState<Preview>(() => cache.get(key) ?? null);

  useEffect(() => {
    const reset = () => { setPreview(null); setEpoch(generation); };
    window.addEventListener("nuvio:thumbnails-cleared", reset);
    return () => window.removeEventListener("nuvio:thumbnails-cleared", reset);
  }, []);

  useEffect(() => {
    const cached = cache.get(key);
    if (cached !== undefined) {
      setPreview(cached);
      return;
    }
    if (file.trashed || !container.current) {
      setPreview(null);
      return;
    }
    let alive = true;
    let near = false;
    const unobserve = observeThumbnail(container.current, (intersecting) => {
      near = intersecting;
      if (near) {
        void preload(key, file.id, () => alive && near, file.kind === "image").then((value) => {
          if (alive) setPreview(value);
        });
      }
    });
    return () => {
      alive = false;
      unobserve();
    };
  }, [key, file.id, file.trashed]);

  const interactive = file.kind === "image" && Boolean(onOpen);
  return <div
    ref={container}
    className={`file-thumbnail ${preview ? "is-ready" : ""} ${preview?.blurred ? "is-blurred" : ""} ${interactive ? "is-interactive" : ""}`}
    role={interactive ? "button" : undefined}
    tabIndex={interactive ? 0 : undefined}
    aria-label={interactive ? `Abrir foto ${file.name}` : undefined}
    onClick={interactive ? (event) => { event.stopPropagation(); onOpen?.(); } : undefined}
    onKeyDown={interactive ? (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        event.stopPropagation();
        onOpen?.();
      }
    } : undefined}
  >
    {preview?.image ? <img src={preview.image} alt={`Miniatura de ${file.name}`} draggable={false} loading="eager" />
      : preview?.text ? <pre aria-label={`Miniatura de ${file.name}`}>{preview.text}</pre> : children}
  </div>;
}
