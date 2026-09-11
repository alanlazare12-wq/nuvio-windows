import { useEffect, useRef, useState, type ReactNode } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { getDocument } from "pdfjs-dist";
import type { CloudFile } from "./types";

type Source = { kind: "image" | "pdf" | "text"; path: string | null; dataUrl: string | null };
type Preview = { image?: string; text?: string } | null;
const cache = new Map<string, Preview>();
const pending = new Map<string, Promise<Preview>>();
const queue: Array<() => Promise<void>> = [];
let active = 0;
let generation = 0;

export function clearThumbnailCache() {
  generation++;
  cache.clear();
  pending.clear();
  window.dispatchEvent(new Event("nuvio:thumbnails-cleared"));
}

function drain() {
  while (active < 2 && queue.length) {
    active++;
    void queue.shift()!().finally(() => { active--; drain(); });
  }
}

async function renderThumbnail(id: string): Promise<Preview> {
  const source = await invoke<Source | null>("prepare_thumbnail", { id });
  if (!source) return null;
  const url = source.dataUrl ?? (source.path ? convertFileSrc(source.path) : null);
  if (!url) return null;
  if (source.kind === "text") {
    const response = await fetch(url);
    if (!response.ok) throw new Error("No se pudo leer la miniatura");
    return { text: (await response.text()).slice(0, 1600) };
  }
  const canvas = document.createElement("canvas");
  const context = canvas.getContext("2d");
  if (!context) return null;
  if (source.kind === "pdf") {
    const task = getDocument({ url });
    try {
      const pdf = await task.promise;
      const page = await pdf.getPage(1);
      const base = page.getViewport({ scale: 1 });
      const viewport = page.getViewport({ scale: Math.min(320 / base.width, 240 / base.height) });
      canvas.width = Math.max(1, Math.floor(viewport.width));
      canvas.height = Math.max(1, Math.floor(viewport.height));
      await page.render({ canvas, canvasContext: context, viewport }).promise;
      return { image: canvas.toDataURL("image/webp", 0.75) };
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
    return { image: canvas.toDataURL("image/webp", 0.75) };
  } finally { bitmap.close(); }
}

function preload(key: string, id: string, isVisible: () => boolean): Promise<Preview> {
  if (cache.has(key)) return Promise.resolve(cache.get(key)!);
  const existing = pending.get(key);
  if (existing) return existing;
  const epoch = generation;
  const result = new Promise<Preview>(resolve => {
    queue.push(async () => {
      if (epoch !== generation || !isVisible()) { pending.delete(key); resolve(null); return; }
      let preview: Preview = null;
      try { preview = await renderThumbnail(id); } catch { /* Keep the file type icon on failure. */ }
      if (epoch === generation) {
        cache.set(key, preview);
        if (cache.size > 256) cache.delete(cache.keys().next().value!);
        pending.delete(key);
        resolve(preview);
      } else { resolve(null); }
    });
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
      { rootMargin: "250px" }
    );
  }
  thumbnailListeners.set(el, cb);
  sharedObserver.observe(el);
  return () => {
    thumbnailListeners.delete(el);
    sharedObserver?.unobserve(el);
  };
}

export function FileThumbnail({ file, children }: { file: CloudFile; children: ReactNode }) {
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
        void preload(key, file.id, () => alive && near).then((value) => {
          if (alive) setPreview(value);
        });
      }
    });
    return () => {
      alive = false;
      unobserve();
    };
  }, [key, file.id, file.trashed]);

  return <div ref={container} className={`file-thumbnail ${preview ? "is-ready" : ""}`}>
    {preview?.image ? <img src={preview.image} alt={`Miniatura de ${file.name}`} draggable={false} loading="lazy" />
      : preview?.text ? <pre aria-label={`Miniatura de ${file.name}`}>{preview.text}</pre> : children}
  </div>;
}
