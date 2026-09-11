use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::timeout;

use serde_json::Value;
use zeroize::Zeroize;

use crate::provider::{ProviderStatus, StorageProvider};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramAuthSnapshot {
    pub stage: String,
    pub message: String,
    pub connected: bool,
    pub account_label: Option<String>,
    pub hint: Option<String>,
    pub qr_link: Option<String>,
    pub is_premium: bool,
    pub qr_svg: Option<String>,
}

impl Default for TelegramAuthSnapshot {
    fn default() -> Self {
        Self {
            stage: "needsCredentials".to_string(),
            message: "Configura tu API ID y API Hash para iniciar TDLib".to_string(),
            connected: false,
            account_label: None,
            hint: None,
            qr_link: None,
            is_premium: false,
            qr_svg: None,
        }
    }
}

pub struct TelegramService {
    pub(crate) sync_progress: Mutex<crate::progress::SyncProgress>,
    pub(crate) catalog_sync_gate: tokio::sync::Mutex<()>,
    pub(crate) client_id: i32,
    database_directory: PathBuf,
    files_directory: PathBuf,
    database_key: String,
    cached: Arc<Mutex<TelegramAuthSnapshot>>,
    pub(crate) sent: Arc<Mutex<HashMap<i64, Result<tdlib_rs::types::Message, String>>>>,
    credentials_path: PathBuf,
}

impl TelegramService {
    pub fn new(app_data_dir: &Path) -> Result<Self, String> {
        let root = app_data_dir.join("telegram");
        let database_directory = root.join("db");
        let files_directory = root.join("files");
        fs::create_dir_all(&database_directory).map_err(|error| error.to_string())?;
        fs::create_dir_all(&files_directory).map_err(|error| error.to_string())?;
        let database_key = load_or_create_database_key(&root)?;

        let client_id = tdlib_rs::create_client();
        let cached = Arc::new(Mutex::new(TelegramAuthSnapshot::default()));
        let sent = Arc::new(Mutex::new(HashMap::new()));
        let auth_updates = cached.clone();
        let send_updates = sent.clone();
        std::thread::Builder::new()
            .name("telegram-receive".into())
            .spawn(move || loop {
                if let Some((update, id)) = tdlib_rs::receive() {
                    if id != client_id {
                        continue;
                    }
                    match update {
                        tdlib_rs::enums::Update::AuthorizationState(v) => {
                            let value =
                                serde_json::to_value(&v.authorization_state).unwrap_or_default();
                            let snapshot = snapshot_from_state(
                                &value,
                                &format!("{:?}", v.authorization_state),
                            );
                            let closed = snapshot.stage == "closed";
                            *auth_updates.lock().expect("auth mutex") = snapshot;
                            if closed {
                                break;
                            }
                        }
                        tdlib_rs::enums::Update::MessageSendSucceeded(v) => {
                            send_updates
                                .lock()
                                .expect("send mutex")
                                .insert(v.old_message_id, Ok(v.message));
                        }
                        tdlib_rs::enums::Update::MessageSendFailed(v) => {
                            send_updates
                                .lock()
                                .expect("send mutex")
                                .insert(v.old_message_id, Err(td_error(v.error)));
                        }
                        _ => {}
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            sync_progress: Mutex::new(crate::progress::SyncProgress::default()),
            catalog_sync_gate: tokio::sync::Mutex::new(()),
            client_id,
            database_directory,
            files_directory,
            database_key,
            cached,
            sent,
            credentials_path: root.join("api-credentials.dpapi"),
        })
    }

    pub async fn initialize(&self, remember_session: bool) -> Result<TelegramAuthSnapshot, String> {
        if !remember_session {
            crate::secrets::remove(&self.credentials_path)?;
        }
        call(tdlib_rs::functions::set_log_verbosity_level(
            0,
            self.client_id,
        ))
        .await?;
        let state = self.refresh().await?;
        if remember_session && state.stage == "needsCredentials" {
            if let Some(bytes) = crate::secrets::load(&self.credentials_path)? {
                let (id, hash): (i32, String) = serde_json::from_slice(&bytes)
                    .map_err(|_| "Credenciales guardadas inválidas")?;
                return self.configure(id, hash, true).await;
            }
        }
        Ok(state)
    }

    pub fn cached_snapshot(&self) -> TelegramAuthSnapshot {
        self.cached
            .lock()
            .expect("telegram auth mutex poisoned")
            .clone()
    }

    pub async fn refresh(&self) -> Result<TelegramAuthSnapshot, String> {
        let state = call(tdlib_rs::functions::get_authorization_state(self.client_id)).await?;
        let value = serde_json::to_value(&state).map_err(|error| error.to_string())?;
        let debug = format!("{state:?}");
        let mut snapshot = snapshot_from_state(&value, &debug);

        if snapshot.connected {
            if let Ok(tdlib_rs::enums::User::User(me)) =
                call(tdlib_rs::functions::get_me(self.client_id)).await
            {
                let full_name = format!("{} {}", me.first_name, me.last_name)
                    .trim()
                    .to_string();
                snapshot.account_label = Some(if full_name.is_empty() {
                    format!("Telegram #{}", me.id)
                } else {
                    full_name
                });
                snapshot.is_premium = me.is_premium;
            }
        }

        *self.cached.lock().expect("telegram auth mutex poisoned") = snapshot.clone();
        Ok(snapshot)
    }

    pub async fn configure(
        &self,
        api_id: i32,
        mut api_hash: String,
        remember_session: bool,
    ) -> Result<TelegramAuthSnapshot, String> {
        if api_id <= 0
            || api_hash.trim().len() != 32
            || !api_hash.trim().bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err("API ID o API Hash inválidos".to_string());
        }

        let result = call(tdlib_rs::functions::set_tdlib_parameters(
            false,
            self.database_directory.to_string_lossy().into_owned(),
            self.files_directory.to_string_lossy().into_owned(),
            self.database_key.clone(),
            true,
            true,
            true,
            false,
            api_id,
            api_hash.clone(),
            "es-MX".to_string(),
            "Nuvio".to_string(),
            std::env::consts::OS.to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
            self.client_id,
        ))
        .await;
        if result.is_ok() {
            if remember_session {
                let bytes = zeroize::Zeroizing::new(
                    serde_json::to_vec(&(api_id, &api_hash)).map_err(|e| e.to_string())?,
                );
                crate::secrets::save(&self.credentials_path, &bytes)?;
            } else {
                crate::secrets::remove(&self.credentials_path)?;
            }
        }
        api_hash.zeroize();
        result?;
        self.refresh().await
    }

    pub async fn submit_phone(&self, mut phone: String) -> Result<TelegramAuthSnapshot, String> {
        let normalized = phone.trim().to_string();
        if !normalized.starts_with('+') || normalized.len() < 8 {
            phone.zeroize();
            return Err("Usa el número en formato internacional, por ejemplo +52 seguido de los 10 dígitos de tu número".to_string());
        }
        let result = call(tdlib_rs::functions::set_authentication_phone_number(
            normalized,
            None,
            self.client_id,
        ))
        .await;
        phone.zeroize();
        result?;
        self.refresh().await
    }

    pub async fn submit_email(&self, mut email: String) -> Result<TelegramAuthSnapshot, String> {
        let value = email.trim().to_string();
        if !value.contains('@') || value.starts_with('@') || value.ends_with('@') {
            email.zeroize();
            return Err("Ingresa un correo válido".to_string());
        }
        let result = call(tdlib_rs::functions::set_authentication_email_address(
            value,
            self.client_id,
        ))
        .await;
        email.zeroize();
        result?;
        self.refresh().await
    }

    pub async fn submit_email_code(
        &self,
        mut code: String,
    ) -> Result<TelegramAuthSnapshot, String> {
        let value = code.trim().to_string();
        if value.len() < 4 {
            code.zeroize();
            return Err("El código de correo es demasiado corto".to_string());
        }
        let authentication = tdlib_rs::enums::EmailAddressAuthentication::Code(
            tdlib_rs::types::EmailAddressAuthenticationCode { code: value },
        );
        let result = call(tdlib_rs::functions::check_authentication_email_code(
            authentication,
            self.client_id,
        ))
        .await;
        code.zeroize();
        result?;
        self.refresh().await
    }

    pub async fn submit_code(&self, mut code: String) -> Result<TelegramAuthSnapshot, String> {
        let value = code.trim().to_string();
        if value.len() < 4 {
            code.zeroize();
            return Err("El código de autenticación es demasiado corto".to_string());
        }
        let result = call(tdlib_rs::functions::check_authentication_code(
            value,
            self.client_id,
        ))
        .await;
        code.zeroize();
        result?;
        self.refresh().await
    }

    pub async fn resend_code(&self) -> Result<TelegramAuthSnapshot, String> {
        let result = call(tdlib_rs::functions::resend_authentication_code(
            None,
            self.client_id,
        ))
        .await;
        result?;
        self.refresh().await
    }

    pub async fn submit_password(
        &self,
        mut password: String,
    ) -> Result<TelegramAuthSnapshot, String> {
        if password.is_empty() {
            return Err("La contraseña no puede estar vacía".to_string());
        }
        let value = password.clone();
        let result = call(tdlib_rs::functions::check_authentication_password(
            value,
            self.client_id,
        ))
        .await;
        password.zeroize();
        result?;
        self.refresh().await
    }

    pub async fn request_qr(&self) -> Result<TelegramAuthSnapshot, String> {
        call(tdlib_rs::functions::request_qr_code_authentication(
            Vec::new(),
            self.client_id,
        ))
        .await?;
        self.refresh().await
    }

    pub async fn register_user(
        &self,
        mut first_name: String,
        mut last_name: String,
    ) -> Result<TelegramAuthSnapshot, String> {
        if first_name.trim().is_empty() {
            return Err("El nombre es obligatorio para terminar el registro".to_string());
        }
        let first = first_name.trim().to_string();
        let last = last_name.trim().to_string();
        let result = call(tdlib_rs::functions::register_user(
            first,
            last,
            false,
            self.client_id,
        ))
        .await;
        first_name.zeroize();
        last_name.zeroize();
        result?;
        self.refresh().await
    }

    pub async fn log_out(&self) -> Result<TelegramAuthSnapshot, String> {
        call(tdlib_rs::functions::log_out(self.client_id)).await?;
        let snapshot = TelegramAuthSnapshot {
            stage: "closed".to_string(),
            message: "Sesión cerrada".to_string(),
            ..TelegramAuthSnapshot::default()
        };
        *self.cached.lock().expect("telegram auth mutex poisoned") = snapshot.clone();
        Ok(snapshot)
    }

    pub async fn forget_session(&self) -> Result<TelegramAuthSnapshot, String> {
        crate::secrets::remove(&self.credentials_path)?;
        let _ = call(tdlib_rs::functions::log_out(self.client_id)).await;
        let deadline = std::time::Instant::now() + Duration::from_secs(12);
        while std::time::Instant::now() < deadline {
            if self.cached_snapshot().stage == "closed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        if self.database_directory.exists() {
            let _ = fs::remove_dir_all(&self.database_directory);
        }
        fs::create_dir_all(&self.database_directory).map_err(|e| e.to_string())?;
        let snapshot = TelegramAuthSnapshot {
            stage: "closed".to_string(),
            message: "Sesión olvidada. Reinicia Nuvio para conectar otra cuenta.".to_string(),
            ..TelegramAuthSnapshot::default()
        };
        *self.cached.lock().expect("telegram auth mutex poisoned") = snapshot.clone();
        Ok(snapshot)
    }
}

impl StorageProvider for TelegramService {
    fn provider_id(&self) -> &'static str {
        "telegram"
    }

    fn status(&self) -> ProviderStatus {
        let snapshot = self.cached_snapshot();
        ProviderStatus {
            connected: snapshot.connected,
            account_label: snapshot.account_label,
            label: snapshot.message,
        }
    }
}

fn snapshot_from_state(value: &Value, debug: &str) -> TelegramAuthSnapshot {
    let json = value.to_string().to_ascii_lowercase();
    let debug_lower = debug.to_ascii_lowercase();
    let state_text = format!("{json} {debug_lower}");
    let mut snapshot = TelegramAuthSnapshot::default();

    if state_text.contains("waittdlibparameters")
        || state_text.contains("authorizationstatewaittdlibparameters")
    {
        snapshot.stage = "needsCredentials".to_string();
        snapshot.message = "TDLib necesita las credenciales de la aplicación".to_string();
    } else if state_text.contains("waitencryptionkey")
        || state_text.contains("authorizationstatewaitencryptionkey")
    {
        snapshot.stage = "initializing".to_string();
        snapshot.message = "Abriendo el almacén local cifrado de Telegram".to_string();
    } else if state_text.contains("waitphonenumber")
        || state_text.contains("authorizationstatewaitphonenumber")
    {
        snapshot.stage = "phone".to_string();
        snapshot.message = "Ingresa el teléfono asociado a tu cuenta".to_string();
    } else if state_text.contains("waitemailaddress")
        || state_text.contains("authorizationstatewaitemailaddress")
    {
        snapshot.stage = "email".to_string();
        snapshot.message = "Telegram solicita un correo de autenticación".to_string();
    } else if state_text.contains("waitemailcode")
        || state_text.contains("authorizationstatewaitemailcode")
    {
        snapshot.stage = "emailCode".to_string();
        snapshot.message = "Ingresa el código enviado al correo".to_string();
        snapshot.hint = find_string(value, &["email_address_pattern"]);
    } else if state_text.contains("waitcode") || state_text.contains("authorizationstatewaitcode") {
        snapshot.stage = "code".to_string();
        snapshot.message = "Ingresa el código enviado por Telegram".to_string();
    } else if state_text.contains("waitpassword")
        || state_text.contains("authorizationstatewaitpassword")
    {
        snapshot.stage = "password".to_string();
        snapshot.message = "Tu cuenta usa verificación en dos pasos".to_string();
        snapshot.hint = find_string(value, &["password_hint", "hint"]);
    } else if state_text.contains("waitotherdeviceconfirmation")
        || state_text.contains("authorizationstatewaitotherdeviceconfirmation")
    {
        snapshot.stage = "qr".to_string();
        snapshot.message = "Escanea el código desde otra sesión de Telegram".to_string();
        snapshot.qr_link = find_string(value, &["link"]);
        snapshot.qr_svg = snapshot
            .qr_link
            .as_ref()
            .and_then(|link| qrcode::QrCode::new(link.as_bytes()).ok())
            .map(|qr| {
                qr.render::<qrcode::render::svg::Color>()
                    .min_dimensions(220, 220)
                    .build()
            });
    } else if state_text.contains("waitregistration")
        || state_text.contains("authorizationstatewaitregistration")
    {
        snapshot.stage = "registration".to_string();
        snapshot.message = "Completa el registro de tu cuenta de Telegram".to_string();
    } else if state_text.contains("authorizationstateready") || debug_lower.contains("ready") {
        snapshot.stage = "ready".to_string();
        snapshot.message = "Telegram conectado".to_string();
        snapshot.connected = true;
    } else if state_text.contains("loggingout") {
        snapshot.stage = "loggingOut".to_string();
        snapshot.message = "Cerrando sesión".to_string();
    } else if state_text.contains("closing") {
        snapshot.stage = "closing".to_string();
        snapshot.message = "Cerrando TDLib".to_string();
    } else if state_text.contains("closed") {
        snapshot.stage = "closed".to_string();
        snapshot.message = "TDLib cerrado".to_string();
    } else {
        snapshot.stage = "initializing".to_string();
        snapshot.message = "Inicializando Telegram".to_string();
    }

    snapshot
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(Value::String(found)) = map.get(*key) {
                    if !found.is_empty() {
                        return Some(found.clone());
                    }
                }
            }
            map.values().find_map(|child| find_string(child, keys))
        }
        Value::Array(values) => values.iter().find_map(|child| find_string(child, keys)),
        _ => None,
    }
}

fn load_or_create_database_key(root: &Path) -> Result<String, String> {
    let key_path = root.join("tdlib.key");
    #[cfg(any(windows, target_os = "android"))]
    if let Some(bytes) = crate::secrets::load(&root.join("tdlib-key.dpapi"))? {
        return String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string());
    }
    if key_path.exists() {
        let key = fs::read_to_string(&key_path).map_err(|error| error.to_string())?;
        let key = key.trim().to_string();
        if key.len() >= 32 {
            #[cfg(any(windows, target_os = "android"))]
            {
                crate::secrets::save(&root.join("tdlib-key.dpapi"), key.as_bytes())?;
                fs::remove_file(&key_path).map_err(|e| e.to_string())?;
            }
            return Ok(key);
        }
    }

    let bytes: [u8; 32] = rand::random();
    let key = hex::encode(bytes);
    #[cfg(any(windows, target_os = "android"))]
    crate::secrets::save(&root.join("tdlib-key.dpapi"), key.as_bytes())?;
    #[cfg(not(any(windows, target_os = "android")))]
    fs::write(&key_path, &key).map_err(|error| error.to_string())?;
    Ok(key)
}

fn td_error(error: tdlib_rs::types::Error) -> String {
    if error.code == 406 {
        return "Telegram rechazó esta operación por una condición interna no mostrable"
            .to_string();
    }
    format!("Telegram {}: {}", error.code, error.message)
}

/// A stalled network must return an actionable error instead of locking the UI.
pub(crate) async fn call<T>(
    request: impl Future<Output = Result<T, tdlib_rs::types::Error>>,
) -> Result<T, String> {
    timeout(Duration::from_secs(45), request)
        .await
        .map_err(|_| {
            "Telegram no respondió en 45 segundos. Revisa tu conexión y vuelve a intentar."
                .to_string()
        })?
        .map_err(td_error)
}
