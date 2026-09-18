import { invoke } from "@tauri-apps/api/core";

let cachedPlatform: string | null = null;

export async function getPlatform(): Promise<string> {
  if (!cachedPlatform) {
    try {
      cachedPlatform = await invoke<string>("platform_name");
    } catch {
      cachedPlatform = "unknown";
    }
  }
  return cachedPlatform;
}

export function readableError(value: unknown): string {
  if (typeof value === "string" && value.trim()) return value;
  if (value instanceof Error && value.message.trim()) return value.message;
  try {
    const message = JSON.stringify(value);
    return message && message !== "null" && message !== '""' ? message : "Ocurrió un error inesperado";
  } catch {
    return "Ocurrió un error inesperado";
  }
}
