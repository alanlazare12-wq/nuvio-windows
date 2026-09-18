import { RefreshCw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { loadPdfJs } from "../pdf";

type PdfPreviewProps = {
  source: string;
  onError: (message: string) => void;
};

export function PdfPreview({ source, onError }: PdfPreviewProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    let destroyTask: (() => void) | null = null;
    setLoading(true);

    void (async () => {
      try {
        const pdfjs = await loadPdfJs();
        if (cancelled) return;

        const task = pdfjs.getDocument({ url: source });
        destroyTask = () => { void task.destroy(); };
        const pdf = await task.promise;
        const page = await pdf.getPage(1);
        if (cancelled) return;

        const base = page.getViewport({ scale: 1 });
        const scale = Math.min(
          1.45,
          4096 / base.width,
          4096 / base.height,
          Math.sqrt(4_000_000 / (base.width * base.height)),
        );
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
      destroyTask?.();
    };
  }, [source, onError]);

  return (
    <div className="pdf-preview">
      {loading && (
        <div className="preview-loading">
          <RefreshCw size={19} /> Renderizando primera página…
        </div>
      )}
      <canvas ref={canvasRef} />
    </div>
  );
}
