let pdfModulePromise: Promise<typeof import("pdfjs-dist")> | null = null;

export function loadPdfJs(): Promise<typeof import("pdfjs-dist")> {
  if (!pdfModulePromise) {
    pdfModulePromise = Promise.all([
      import("pdfjs-dist"),
      import("pdfjs-dist/build/pdf.worker.min.mjs?url"),
    ]).then(([pdfjs, worker]) => {
      pdfjs.GlobalWorkerOptions.workerSrc = worker.default;
      return pdfjs;
    });
  }
  return pdfModulePromise;
}
