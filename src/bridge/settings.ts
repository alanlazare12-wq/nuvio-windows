import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import type { AppSettings } from "../types";

export function updateSetting(
  key: keyof AppSettings | string,
  value: string,
): Promise<AppSettings> {
  return invoke<AppSettings>("update_setting", { key, value });
}

export async function exportDiagnostics(): Promise<boolean> {
  const destination = await save({
    title: "Exportar diagnóstico de Nuvio",
    defaultPath: `nuvio-diagnostics-${new Date().toISOString().slice(0, 10)}.json`,
    filters: [{ name: "JSON", extensions: ["json"] }],
  });
  if (!destination) return false;
  await invoke("export_diagnostics", { destination });
  return true;
}
