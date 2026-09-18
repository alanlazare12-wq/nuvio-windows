#[derive(Debug, Clone)]
pub struct ProviderStatus {
    pub connected: bool,
    pub account_label: Option<String>,
    pub label: String,
}

pub trait StorageProvider: Send + Sync {
    fn provider_id(&self) -> &'static str;
    fn status(&self) -> ProviderStatus;
    fn max_upload_bytes(&self) -> u64;
}
