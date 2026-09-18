import { invoke } from "@tauri-apps/api/core";

export async function setFavorite(id: string, favorite: boolean): Promise<void> {
  await invoke("set_favorite", { id, favorite });
}

export function createFolder(name: string, parentId?: string | null): Promise<string> {
  return invoke<string>("create_folder", { name, parentId: parentId ?? null });
}

export function renameFolder(id: string, name: string): Promise<void> {
  return invoke("rename_folder", { id, name });
}

export function moveFolder(id: string, parentId?: string | null): Promise<void> {
  return invoke("move_folder", { id, parentId: parentId ?? null });
}

export function deleteFolder(id: string): Promise<void> {
  return invoke("delete_folder", { id });
}

export function moveFilesToFolder(ids: string[], folderId?: string | null): Promise<number> {
  return invoke<number>("move_files_to_folder", { ids, folderId: folderId ?? null });
}

export function setTrashed(id: string, trashed: boolean): Promise<void> {
  return invoke("set_trashed", { id, trashed });
}

export function setTrashedMany(ids: string[], trashed: boolean): Promise<number> {
  return invoke<number>("set_trashed_many", { ids, trashed });
}

export function deleteFilesPermanently(ids: string[]): Promise<number> {
  return invoke<number>("delete_files_permanently", { ids });
}

export function emptyTrash(): Promise<number> {
  return invoke<number>("empty_trash");
}
