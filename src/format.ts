export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const index = Math.min(
    Math.floor(Math.log(bytes) / Math.log(1024)),
    units.length - 1,
  );
  const value = bytes / 1024 ** index;
  return `${value >= 10 || index < 2
    ? value.toFixed(index === 0 ? 0 : 1)
    : value.toFixed(2)} ${units[index]}`;
}

export function formatSpeed(bytesPerSecond: number): string {
  return bytesPerSecond > 0 ? `${formatBytes(bytesPerSecond)}/s` : "—";
}

export function formatEta(
  seconds?: number | null,
  calculating = false,
): string {
  if (seconds == null || !Number.isFinite(seconds)) {
    return calculating ? "Calculando tiempo restante…" : "—";
  }
  if (seconds <= 0) return "Menos de 1 s";

  const rounded = Math.ceil(seconds);
  const hours = Math.floor(rounded / 3600);
  const minutes = Math.floor((rounded % 3600) / 60);
  const secs = rounded % 60;
  if (hours > 0) return `${hours} h ${minutes} min`;
  if (minutes > 0) return `${minutes} min ${secs} s`;
  return `${secs} s`;
}
