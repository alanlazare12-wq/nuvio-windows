use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudFile {
    pub id: String,
    pub name: String,
    pub extension: String,
    pub kind: String,
    pub size_bytes: i64,
    pub updated_at: String,
    pub favorite: bool,
    pub trashed: bool,
    pub folder: String,
    pub folder_id: Option<String>,
    pub tags: Vec<String>,
    pub provider: String,
    pub telegram_message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudFolder {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub trashed: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub file_count: usize,
    pub child_count: usize,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferJob {
    pub id: String,
    pub file_name: String,
    pub direction: String,
    pub progress: u8,
    pub status: String,
    pub phase: String,
    pub speed_label: String,
    pub processed_bytes: i64,
    pub total_bytes: i64,
    pub speed_bps: i64,
    pub eta_seconds: Option<i64>,
    pub attempts: i32,
    pub max_attempts: i32,
    pub error: Option<String>,
    pub can_pause: bool,
    pub can_retry: bool,
    pub can_cancel: bool,
    pub started_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSummary {
    pub total: usize,
    pub completed: usize,
    pub pending: usize,
    pub failed: usize,
    pub active: usize,
    pub processed_bytes: i64,
    pub total_bytes: i64,
    pub speed_bps: i64,
    pub eta_seconds: Option<i64>,
    pub cache_bytes: i64,
    pub cache_limit_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub preparation_concurrency: usize,
    pub upload_concurrency: usize,
    pub download_concurrency: usize,
    pub cache_limit_bytes: i64,
    pub remember_session: bool,
    pub conflict_policy: String,
    pub speed_limit_bps: Option<i64>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            preparation_concurrency: 4,
            upload_concurrency: 8,
            download_concurrency: 2,
            cache_limit_bytes: 2 * 1024 * 1024 * 1024,
            remember_session: false,
            conflict_policy: "skip".to_string(),
            speed_limit_bps: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardData {
    pub sync_progress: crate::progress::SyncProgress,
    pub files: Vec<CloudFile>,
    pub folders: Vec<CloudFolder>,
    pub transfers: Vec<TransferJob>,
    pub transfer_history: Vec<TransferJob>,
    pub total_bytes: i64,
    pub file_count: usize,
    pub favorite_count: usize,
    pub recent_count: usize,
    pub telegram_connected: bool,
    pub telegram_account_label: Option<String>,
    pub provider_status: String,
    pub queue_summary: QueueSummary,
    pub settings: AppSettings,
    pub is_premium: bool,
}
