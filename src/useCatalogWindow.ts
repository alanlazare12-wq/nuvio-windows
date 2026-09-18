import { useEffect, useMemo, useRef, useState } from "react";
import type { CloudFile } from "./types";

type CatalogView = "grid" | "list";

type VirtualRange = {
  start: number;
  end: number;
  top: number;
  bottom: number;
};

const VIRTUALIZE_AFTER = 240;
const OVERSCAN_ROWS = 4;

function gridColumns() {
  if (window.innerWidth <= 720) return 2;
  if (window.innerWidth <= 1180) return 3;
  return 4;
}

function defaultItemHeight(view: CatalogView) {
  if (view === "list") return window.innerWidth <= 720 ? 64 : 58;
  return window.innerWidth <= 720 ? 174 : 190;
}

function itemGap(view: CatalogView) {
  if (view === "list") return window.innerWidth <= 720 ? 8 : 0;
  return window.innerWidth <= 720 ? 10 : 13;
}

export function useCatalogWindow(files: CloudFile[], view: CatalogView) {
  const hostRef = useRef<HTMLDivElement>(null);
  const measuredHeightRef = useRef(0);
  const [range, setRange] = useState<VirtualRange>({
    start: 0,
    end: files.length,
    top: 0,
    bottom: 0,
  });
  const enabled = files.length > VIRTUALIZE_AFTER;

  useEffect(() => {
    if (!enabled) {
      measuredHeightRef.current = 0;
      setRange({ start: 0, end: files.length, top: 0, bottom: 0 });
      return;
    }

    let frame = 0;
    const contentScroll = document.querySelector<HTMLElement>(".content-scroll");

    const compute = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const host = hostRef.current;
        if (!host) return;

        const columns = view === "grid" ? gridColumns() : 1;
        const gap = itemGap(view);
        const sample = host.querySelector<HTMLElement>(
          view === "grid" ? ".file-card" : ".file-row",
        );
        const measured = sample?.getBoundingClientRect().height ?? 0;
        if (measured > 20) measuredHeightRef.current = measured;
        const itemHeight = measuredHeightRef.current || defaultItemHeight(view);
        const stride = Math.max(1, itemHeight + gap);
        const totalRows = Math.ceil(files.length / columns);

        const hostRect = host.getBoundingClientRect();
        const desktopViewport = contentScroll?.getBoundingClientRect();
        const viewportTop = window.innerWidth <= 720 ? 0 : (desktopViewport?.top ?? 0);
        const viewportBottom = window.innerWidth <= 720
          ? window.innerHeight
          : (desktopViewport?.bottom ?? window.innerHeight);

        const localTop = Math.max(0, viewportTop - hostRect.top);
        const localBottom = Math.max(
          localTop,
          Math.min(hostRect.height, viewportBottom - hostRect.top),
        );

        const startRow = Math.max(0, Math.floor(localTop / stride) - OVERSCAN_ROWS);
        const endRow = Math.min(
          totalRows,
          Math.max(startRow + 1, Math.ceil(localBottom / stride) + OVERSCAN_ROWS),
        );
        const next: VirtualRange = {
          start: Math.min(files.length, startRow * columns),
          end: Math.min(files.length, endRow * columns),
          top: startRow * stride,
          bottom: Math.max(0, (totalRows - endRow) * stride),
        };

        setRange((current) =>
          current.start === next.start
          && current.end === next.end
          && Math.abs(current.top - next.top) < 1
          && Math.abs(current.bottom - next.bottom) < 1
            ? current
            : next,
        );
      });
    };

    compute();
    window.addEventListener("scroll", compute, { passive: true });
    window.addEventListener("resize", compute, { passive: true });
    contentScroll?.addEventListener("scroll", compute, { passive: true });
    const resizeObserver = new ResizeObserver(compute);
    resizeObserver.observe(hostRef.current ?? document.documentElement);

    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("scroll", compute);
      window.removeEventListener("resize", compute);
      contentScroll?.removeEventListener("scroll", compute);
      resizeObserver.disconnect();
    };
  }, [enabled, files.length, view]);

  const visibleFiles = useMemo(
    () => enabled ? files.slice(range.start, range.end) : files,
    [enabled, files, range.end, range.start],
  );

  return {
    hostRef,
    visibleFiles,
    topSpacer: enabled ? range.top : 0,
    bottomSpacer: enabled ? range.bottom : 0,
  };
}
