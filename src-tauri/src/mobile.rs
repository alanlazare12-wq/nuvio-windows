//! Android system storage and Keystore. These commands are called only by Rust.
#[cfg(target_os = "android")]
use serde::{de::DeserializeOwned, Deserialize};
#[cfg(target_os = "android")]
use std::{path::Path, sync::OnceLock};

#[cfg(target_os = "android")]
static HANDLE: OnceLock<tauri::plugin::PluginHandle<tauri::Wry>> = OnceLock::new();

pub fn init() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("nuvio-mobile")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            HANDLE
                .set(_api.register_android_plugin("com.nuvio.drive", "NuvioMobilePlugin")?)
                .map_err(|_| "Android ya está inicializado")?;
            Ok(())
        })
        .build()
}

#[cfg(target_os = "android")]
fn call<T: DeserializeOwned>(command: &str, args: serde_json::Value) -> Result<T, String> {
    HANDLE
        .get()
        .ok_or("Android aún no está listo")?
        .run_mobile_plugin(command, args)
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "android")]
pub fn protect(input: &[u8], encrypt: bool) -> Result<Vec<u8>, String> {
    #[derive(Deserialize)]
    struct Response {
        data: String,
    }
    let result: Response = call(
        "protectSecret",
        serde_json::json!({"data": hex::encode(input), "encrypt": encrypt}),
    )?;
    hex::decode(result.data).map_err(|e| e.to_string())
}

#[cfg(target_os = "android")]
pub async fn pick_directory() -> Result<Option<String>, String> {
    #[derive(Deserialize)]
    struct Response {
        uri: Option<String>,
    }
    let result: Response = HANDLE
        .get()
        .ok_or("Android aún no está listo")?
        .run_mobile_plugin_async("pickDirectory", serde_json::json!({}))
        .await
        .map_err(|e| e.to_string())?;
    Ok(result.uri)
}

#[cfg(target_os = "android")]
pub fn directory_names(uri: &str) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct Response {
        names: Vec<String>,
    }
    call::<Response>("directoryNames", serde_json::json!({"uri": uri})).map(|r| r.names)
}

#[cfg(target_os = "android")]
pub fn publish_download(
    source: &Path,
    tree: &str,
    name: &str,
    policy: &str,
    sha256: &str,
    size: i64,
) -> Result<(), String> {
    let _: serde_json::Value = call(
        "publishDownload",
        serde_json::json!({
            "source": source, "uri": tree, "name": name, "policy": policy, "sha256": sha256, "size": size
        }),
    )?;
    Ok(())
}

#[cfg(target_os = "android")]
pub fn write_document(uri: &str, bytes: &[u8]) -> Result<(), String> {
    let _: serde_json::Value = call(
        "writeDocument",
        serde_json::json!({"uri": uri, "data": hex::encode(bytes)}),
    )?;
    Ok(())
}

pub fn background() -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let _: serde_json::Value = call("backgroundApp", serde_json::json!({}))?;
    }
    Ok(())
}

#[cfg(target_os = "android")]
pub async fn pick_upload_files() -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct Response {
        paths: Vec<String>,
    }
    let result: Response = HANDLE
        .get()
        .ok_or("Android aún no está listo")?
        .run_mobile_plugin_async("pickUploadFiles", serde_json::json!({}))
        .await
        .map_err(|e| e.to_string())?;
    Ok(result.paths)
}

#[derive(Debug, Clone, serde::Deserialize)]
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
#[serde(rename_all = "camelCase")]
pub struct StagedContent {
    pub path: String,
    pub size_bytes: i64,
    pub sha256: String,
}

#[cfg(target_os = "android")]
pub async fn stage_content_uri(uri: &str) -> Result<StagedContent, String> {
    HANDLE
        .get()
        .ok_or("Android aún no está listo")?
        .run_mobile_plugin_async("stageContentUri", serde_json::json!({"uri": uri}))
        .await
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "android")]
pub async fn delete_verified_document(uri: &str, sha256: &str, size: i64) -> Result<bool, String> {
    #[derive(Deserialize)]
    struct Response {
        deleted: bool,
    }
    let result: Response = HANDLE
        .get()
        .ok_or("Android aún no está listo")?
        .run_mobile_plugin_async(
            "deleteVerifiedDocument",
            serde_json::json!({"uri": uri, "sha256": sha256, "size": size}),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(result.deleted)
}

#[cfg(target_os = "android")]
pub async fn pick_upload_directory() -> Result<Option<crate::DirectoryUploadPlan>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawResponse {
        cancelled: Option<bool>,
        root_name: Option<String>,
        folders: Option<Vec<String>>,
        files: Option<Vec<crate::ScannedUploadFile>>,
        total_bytes: Option<u64>,
    }
    let result: RawResponse = HANDLE
        .get()
        .ok_or("Android aún no está listo")?
        .run_mobile_plugin_async("pickUploadDirectory", serde_json::json!({}))
        .await
        .map_err(|e| e.to_string())?;

    if result.cancelled.unwrap_or(false) || result.root_name.is_none() {
        return Ok(None);
    }

    let folders = result.folders.unwrap_or_default();
    let files = result.files.unwrap_or_default();
    crate::validate_directory_upload_limits(files.len(), folders.len(), 0)?;

    Ok(Some(crate::DirectoryUploadPlan {
        root_name: result.root_name.unwrap_or_else(|| "Carpeta".to_string()),
        folders,
        files,
        total_bytes: result.total_bytes.unwrap_or(0),
    }))
}

#[allow(dead_code)]
#[cfg(not(target_os = "android"))]
pub async fn pick_upload_files() -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

#[allow(dead_code)]
#[cfg(not(target_os = "android"))]
pub async fn pick_upload_directory() -> Result<Option<crate::DirectoryUploadPlan>, String> {
    Ok(None)
}

#[allow(dead_code)]
#[cfg(not(target_os = "android"))]
pub async fn stage_content_uri(_uri: &str) -> Result<StagedContent, String> {
    Err("Los URI de Android sólo están soportados en Android".into())
}

pub fn update_sync_notification(_data: &serde_json::Value) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let _: serde_json::Value = call("updateSyncNotification", _data.clone())?;
    }
    Ok(())
}

pub fn update_upload_notification(_data: &serde_json::Value) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let _: serde_json::Value = call("updateUploadNotification", _data.clone())?;
    }
    Ok(())
}

pub fn clear_notification(_id: i32) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let _: serde_json::Value = call("clearNotification", serde_json::json!({ "id": _id }))?;
    }
    Ok(())
}
