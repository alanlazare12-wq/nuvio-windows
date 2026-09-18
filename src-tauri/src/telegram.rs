use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
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
    pub timeout: Option<i32>,
    pub code_type: Option<String>,
    pub next_code_type: Option<String>,
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
            timeout: None,
            code_type: None,
            next_code_type: None,
        }
    }
}

pub struct TelegramService {
    pub(crate) sync_progress: Mutex<crate::progress::SyncProgress>,
    pub(crate) catalog_sync_gate: tokio::sync::Mutex<()>,
    client_id_atomic: Arc<AtomicI32>,
    database_directory: PathBuf,
    files_directory: PathBuf,
    database_key: String,
    cached: Arc<Mutex<TelegramAuthSnapshot>>,
    pub(crate) sent: Arc<Mutex<HashMap<i64, Result<tdlib_rs::types::Message, String>>>>,
    credentials_path: PathBuf,
    saved_api: Mutex<Option<(i32, String)>>,
}

impl TelegramService {
    pub(crate) fn client_id(&self) -> i32 {
        self.client_id_atomic.load(Ordering::SeqCst)
    }
    pub fn new(app_data_dir: &Path) -> Result<Self, String> {
        let root = app_data_dir.join("telegram");
        let database_directory = root.join("db");
        let files_directory = root.join("files");
        fs::create_dir_all(&database_directory).map_err(|error| error.to_string())?;
        fs::create_dir_all(&files_directory).map_err(|error| error.to_string())?;
        let database_key = load_or_create_database_key(&root)?;

        let client_id_atomic = Arc::new(AtomicI32::new(tdlib_rs::create_client()));
        let cached = Arc::new(Mutex::new(TelegramAuthSnapshot::default()));
        let sent = Arc::new(Mutex::new(HashMap::new()));
        let auth_updates = cached.clone();
        let send_updates = sent.clone();
        let client_id_recv = client_id_atomic.clone();
        std::thread::Builder::new()
            .name("telegram-receive".into())
            .spawn(move || loop {
                if let Some((update, id)) = tdlib_rs::receive() {
                    if id != client_id_recv.load(Ordering::SeqCst) {
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
                            *auth_updates.lock().expect("auth mutex") = snapshot;
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
            client_id_atomic,
            database_directory,
            files_directory,
            database_key,
            cached,
            sent,
            credentials_path: root.join("api-credentials.dpapi"),
            saved_api: Mutex::new(None),
        })
    }

    pub async fn initialize(&self, remember_session: bool) -> Result<TelegramAuthSnapshot, String> {
        if !remember_session {
            crate::secrets::remove(&self.credentials_path)?;
        }
        call(tdlib_rs::functions::set_log_verbosity_level(
            0,
            self.client_id(),
        ))
        .await?;
        let state = self.refresh().await?;
        if remember_session && state.stage == "needsCredentials" {
            if let Some(bytes) = crate::secrets::load(&self.credentials_path)? {
                let (id, hash): (i32, String) = serde_json::from_slice(&bytes)
                    .map_err(|_| "Credenciales guardadas inválidas")?;
                *self.saved_api.lock().unwrap() = Some((id, hash.clone()));
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
        let state = call(tdlib_rs::functions::get_authorization_state(
            self.client_id(),
        ))
        .await?;
        let value = serde_json::to_value(&state).map_err(|error| error.to_string())?;
        let debug = format!("{state:?}");
        let mut snapshot = snapshot_from_state(&value, &debug);

        if snapshot.connected {
            if let Ok(tdlib_rs::enums::User::User(me)) =
                call(tdlib_rs::functions::get_me(self.client_id())).await
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
            self.client_id(),
        ))
        .await;
        if result.is_ok() {
            *self.saved_api.lock().unwrap() = Some((api_id, api_hash.clone()));
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

    pub async fn reset_to_phone(&self) -> Result<TelegramAuthSnapshot, String> {
        let state = self.refresh().await?;
        if state.stage != "qr" {
            return Ok(state);
        }
        let saved = self
            .saved_api
            .lock()
            .map_err(|_| "No se pudo leer la configuración de Telegram")?
            .clone();
        let (api_id, api_hash) =
            saved.ok_or("No se pudo recuperar la configuración de Telegram. Reinicia Nuvio.")?;
        call(tdlib_rs::functions::close(self.client_id())).await?;
        for _ in 0..200 {
            if self.cached_snapshot().stage == "closed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if self.cached_snapshot().stage != "closed" {
            return Err(
                "Telegram sigue cerrando la conexión. Reinicia Nuvio si el cierre no termina."
                    .into(),
            );
        }
        // A QR challenge is transient. Reopen the same encrypted store only
        // after TDLib has released it; never delete user data to change methods.
        let new_id = tdlib_rs::create_client();
        self.client_id_atomic.store(new_id, Ordering::SeqCst);
        call(tdlib_rs::functions::set_log_verbosity_level(0, new_id)).await?;
        call(tdlib_rs::functions::set_tdlib_parameters(
            false,
            self.database_directory.to_string_lossy().into_owned(),
            self.files_directory.to_string_lossy().into_owned(),
            self.database_key.clone(),
            true,
            true,
            true,
            false,
            api_id,
            api_hash,
            "es-MX".into(),
            "Nuvio".into(),
            std::env::consts::OS.into(),
            env!("CARGO_PKG_VERSION").into(),
            new_id,
        ))
        .await?;
        self.refresh().await
    }

    pub async fn submit_phone(
        &self,
        mut phone: String,
        delivery: &str,
    ) -> Result<TelegramAuthSnapshot, String> {
        let settings = phone_delivery_settings(delivery)?;
        let normalized: String = phone.chars().filter(|c| !c.is_whitespace()).collect();
        if !normalized.starts_with('+')
            || !(8..=16).contains(&normalized.len())
            || !normalized[1..].bytes().all(|b| b.is_ascii_digit())
        {
            phone.zeroize();
            return Err("Usa el número en formato internacional, por ejemplo +52 seguido de los 10 dígitos de tu número".to_string());
        }
        let result = call(tdlib_rs::functions::set_authentication_phone_number(
            normalized,
            settings,
            self.client_id(),
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
        if self.refresh().await?.stage != "email" {
            email.zeroize();
            return Err("Telegram todavía no solicita un correo. Primero introduce tu teléfono y sigue el canal indicado.".to_string());
        }
        let result = call(tdlib_rs::functions::set_authentication_email_address(
            value,
            self.client_id(),
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
        if value.is_empty() {
            code.zeroize();
            return Err("Introduce el código enviado al correo".to_string());
        }
        let authentication = tdlib_rs::enums::EmailAddressAuthentication::Code(
            tdlib_rs::types::EmailAddressAuthenticationCode { code: value },
        );
        let result = call(tdlib_rs::functions::check_authentication_email_code(
            authentication,
            self.client_id(),
        ))
        .await;
        code.zeroize();
        result?;
        self.refresh().await
    }

    pub async fn submit_code(&self, mut code: String) -> Result<TelegramAuthSnapshot, String> {
        let value = code.trim().to_string();
        if value.is_empty() {
            code.zeroize();
            return Err("Introduce el código, palabra o frase de verificación".to_string());
        }
        let result = call(tdlib_rs::functions::check_authentication_code(
            value,
            self.client_id(),
        ))
        .await;
        code.zeroize();
        result?;
        self.refresh().await
    }

    pub async fn resend_code(
        &self,
        delivery: Option<&str>,
    ) -> Result<TelegramAuthSnapshot, String> {
        let state = self.refresh().await?;
        if state.stage != "emailCode" && (state.stage != "code" || state.next_code_type.is_none()) {
            return Err("Telegram no ofrece otro canal de reenvío en este momento.".to_string());
        }
        validate_requested_delivery(&state, delivery)?;
        let result = call(tdlib_rs::functions::resend_authentication_code(
            Some(tdlib_rs::enums::ResendCodeReason::UserRequest),
            self.client_id(),
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
            self.client_id(),
        ))
        .await;
        password.zeroize();
        result?;
        self.refresh().await
    }

    pub async fn request_qr(&self) -> Result<TelegramAuthSnapshot, String> {
        let current = self.cached_snapshot().stage;
        if current == "qr" {
            return self.refresh().await;
        }
        if current != "phone" {
            self.reset_to_phone().await?;
        }
        call(tdlib_rs::functions::request_qr_code_authentication(
            Vec::new(),
            self.client_id(),
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
            self.client_id(),
        ))
        .await;
        first_name.zeroize();
        last_name.zeroize();
        result?;
        self.refresh().await
    }

    pub async fn log_out(&self) -> Result<TelegramAuthSnapshot, String> {
        call(tdlib_rs::functions::log_out(self.client_id())).await?;
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
        let _ = call(tdlib_rs::functions::log_out(self.client_id())).await;
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

    fn max_upload_bytes(&self) -> u64 {
        if self.cached_snapshot().is_premium {
            4_000_000_000
        } else {
            2_000_000_000
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
        snapshot.message = "Ingresa el código de verificación".to_string();
        apply_code_metadata(&mut snapshot, value);
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
    let msg_lower = error.message.to_ascii_lowercase();
    if error.code == 400
        && (msg_lower.contains("can't be resend")
            || msg_lower.contains("cannot be resend")
            || msg_lower.contains("can't be resent"))
    {
        return "Telegram requiere esperar a que finalice la cuenta regresiva antes de solicitar el código por SMS o llamada telefónica.".to_string();
    }
    if error.code == 400 && msg_lower.contains("phone_code_expired") {
        return "El código de verificación ha expirado. Solicita un nuevo código o reenvío."
            .to_string();
    }
    if error.code == 400 && msg_lower.contains("phone_number_invalid") {
        return "El número de teléfono no es válido. Ingresa tu número en formato internacional (+ y lada).".to_string();
    }
    if error.code == 400 && msg_lower.contains("phone_number_banned") {
        return "Este número de teléfono ha sido suspendido o bloqueado por Telegram.".to_string();
    }
    if error.code == 420
        || error.code == 429
        || msg_lower.contains("flood_wait")
        || msg_lower.contains("too many requests")
    {
        return "Demasiadas solicitudes a Telegram. Por favor espera unos minutos antes de intentar de nuevo.".to_string();
    }
    if error.code == 406 {
        return "Telegram rechazó esta operación por una condición interna no mostrable."
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_from_state_code_info() {
        let json = serde_json::json!({
            "@type": "authorizationStateWaitCode",
            "code_info": {
                "phone_number": "+521234567890",
                "type": {
                    "@type": "authenticationCodeTypeTelegramMessage",
                    "length": 5
                },
                "next_type": {
                    "@type": "authenticationCodeTypeSms",
                    "length": 5
                },
                "timeout": 60
            }
        });
        let snapshot = snapshot_from_state(&json, "WaitCode");
        assert_eq!(snapshot.stage, "code");
        assert_eq!(snapshot.timeout, Some(60));
        assert_eq!(snapshot.code_type, Some("telegram".to_string()));
        assert_eq!(snapshot.next_code_type, Some("sms".to_string()));
        assert!(snapshot.hint.unwrap().contains("Telegram"));
    }

    #[test]
    fn test_snapshot_from_state_sms_info() {
        let json = serde_json::json!({
            "@type": "authorizationStateWaitCode",
            "code_info": {
                "phone_number": "+521234567890",
                "type": {
                    "@type": "authenticationCodeTypeSms",
                    "length": 5
                },
                "next_type": {
                    "@type": "authenticationCodeTypeCall",
                    "length": 5
                },
                "timeout": 120
            }
        });
        let snapshot = snapshot_from_state(&json, "WaitCode");
        assert_eq!(snapshot.stage, "code");
        assert_eq!(snapshot.timeout, Some(120));
        assert_eq!(snapshot.code_type, Some("sms".to_string()));
        assert_eq!(snapshot.next_code_type, Some("call".to_string()));
        assert!(snapshot.hint.unwrap().contains("SMS"));
    }

    #[test]
    fn test_td_error_humanization() {
        let err_resend = tdlib_rs::types::Error {
            code: 400,
            message: "Authentication code can't be resend".to_string(),
        };
        assert!(td_error(err_resend).contains("cuenta regresiva"));
    }
}

fn find_field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => map
            .get(key)
            .or_else(|| map.values().find_map(|v| find_field(v, key))),
        Value::Array(items) => items.iter().find_map(|v| find_field(v, key)),
        _ => None,
    }
}

fn code_channel(value: &Value) -> Option<&'static str> {
    // Inspect only this code type. The next delivery type must never determine
    // where the code that was already sent can be found.
    let name = value
        .get("@type")
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
        .or_else(|| {
            value
                .as_object()
                .and_then(|m| m.keys().next().map(String::as_str))
        })?
        .to_ascii_lowercase();
    if name.contains("telegrammessage") {
        Some("telegram")
    } else if name.contains("missedcall") {
        Some("missedCall")
    } else if name.contains("flashcall") {
        Some("flashCall")
    } else if name.contains("call") {
        Some("call")
    } else if name.contains("smsword") {
        Some("smsWord")
    } else if name.contains("smsphrase") {
        Some("smsPhrase")
    } else if name.contains("sms") {
        Some("sms")
    } else if name.contains("fragment") {
        Some("fragment")
    } else {
        None
    }
}

fn apply_code_metadata(snapshot: &mut TelegramAuthSnapshot, value: &Value) {
    let Some(info) = find_field(value, "code_info").or_else(|| find_field(value, "codeInfo"))
    else {
        return;
    };
    snapshot.timeout = info
        .get("timeout")
        .and_then(Value::as_i64)
        .map(|v| v.clamp(0, i32::MAX as i64) as i32);
    let current = info.get("type").and_then(code_channel);
    snapshot.code_type = current.map(str::to_owned);
    snapshot.next_code_type = info
        .get("next_type")
        .or_else(|| info.get("nextType"))
        .and_then(code_channel)
        .map(str::to_owned);
    snapshot.hint = Some(match current {
        Some("telegram") => "Revisa el mensaje de código en tu otra sesión de Telegram.",
        Some("sms") => "Revisa el SMS enviado a tu teléfono.",
        Some("smsWord") => "Escribe la palabra enviada por SMS.",
        Some("smsPhrase") => "Escribe la frase enviada por SMS.",
        Some("call") => "Introduce el código dictado en la llamada telefónica.",
        Some("missedCall") => "Introduce los últimos dígitos del número de la llamada perdida indicados por Telegram.",
        Some("flashCall") => "Introduce el número de teléfono de la llamada de verificación.",
        Some("fragment") => "Consulta el código de verificación en Fragment.",
        _ => "Sigue las indicaciones de verificación de Telegram.",
    }.to_string());
    if current == Some("missedCall") {
        if let Some(details) = info.get("type") {
            let length = find_field(details, "length")
                .and_then(Value::as_u64)
                .filter(|length| (1..=32).contains(length));
            if let Some(length) = length {
                let prefix = find_string(details, &["phone_number_prefix"]);
                snapshot.hint = Some(match prefix {
                    Some(prefix) => format!("Introduce los últimos {length} dígitos del número que te llamó (empieza por {prefix})."),
                    None => format!("Introduce los últimos {length} dígitos del número que te llamó."),
                });
            }
        }
    }
}

#[cfg(test)]
mod auth_channel_regressions {
    use super::*;
    #[test]
    fn current_channel_is_independent_of_every_next_channel() {
        let channels = [
            ("TelegramMessage", "telegram"),
            ("Sms", "sms"),
            ("Call", "call"),
            ("MissedCall", "missedCall"),
            ("SmsWord", "smsWord"),
            ("SmsPhrase", "smsPhrase"),
        ];
        for (current, expected) in channels {
            for (next, expected_next) in channels {
                let value = serde_json::json!({"@type":"authorizationStateWaitCode", "code_info": {
                    "type":{"@type":format!("authenticationCodeType{current}")},
                    "next_type":{"@type":format!("authenticationCodeType{next}")}, "timeout":30
                }});
                let snapshot = snapshot_from_state(&value, "WaitCode");
                assert_eq!(snapshot.code_type.as_deref(), Some(expected));
                assert_eq!(snapshot.next_code_type.as_deref(), Some(expected_next));
                assert_eq!(snapshot.timeout, Some(30));
            }
        }
    }
    #[test]
    fn actual_tdlib_serialization_keeps_delivery_metadata() {
        let json = serde_json::json!({"@type":"authorizationStateWaitCode", "code_info": {
            "phone_number":"15550000000", "type":{"@type":"authenticationCodeTypeCall","length":5},
            "next_type":{"@type":"authenticationCodeTypeSms","length":5}, "timeout":12
        }});
        let state: tdlib_rs::enums::AuthorizationState = serde_json::from_value(json).unwrap();
        let snapshot = snapshot_from_state(
            &serde_json::to_value(&state).unwrap(),
            &format!("{state:?}"),
        );
        assert_eq!(snapshot.code_type.as_deref(), Some("call"));
        assert_eq!(snapshot.next_code_type.as_deref(), Some("sms"));
        assert_eq!(snapshot.timeout, Some(12));
    }
    #[test]
    fn absent_next_channel_does_not_offer_sms_or_call() {
        let value = serde_json::json!({"@type":"authorizationStateWaitCode", "code_info": {
            "type":{"@type":"authenticationCodeTypeTelegramMessage"},"next_type":null,"timeout":0
        }});
        let snapshot = snapshot_from_state(&value, "WaitCode");
        assert_eq!(snapshot.next_code_type, None);
        assert!(!snapshot.hint.unwrap().contains("SMS"));
    }
    #[test]
    fn email_code_keeps_its_own_state() {
        let value = serde_json::json!({"@type":"authorizationStateWaitEmailCode", "code_info":{"email_address_pattern":"a***@example.com","length":6}});
        let snapshot = snapshot_from_state(&value, "WaitEmailCode");
        assert_eq!(snapshot.stage, "emailCode");
        assert_eq!(snapshot.hint.as_deref(), Some("a***@example.com"));
        assert_eq!(snapshot.code_type, None);
    }
}

fn delivery_family(kind: &str) -> &str {
    match kind {
        "sms" | "smsWord" | "smsPhrase" => "sms",
        "call" | "missedCall" | "flashCall" => "call",
        other => other,
    }
}

fn phone_delivery_settings(
    delivery: &str,
) -> Result<Option<tdlib_rs::types::PhoneNumberAuthenticationSettings>, String> {
    match delivery {
        "telegram" | "sms" | "email" => Ok(None),
        "call" => Ok(Some(tdlib_rs::types::PhoneNumberAuthenticationSettings {
            allow_flash_call: false,
            allow_missed_call: true,
            is_current_phone_number: false,
            has_unknown_phone_number: false,
            allow_sms_retriever_api: false,
            firebase_authentication_settings: None,
            authentication_tokens: Vec::new(),
        })),
        _ => Err("Elige SMS, correo electrónico, llamada o Telegram.".into()),
    }
}

fn validate_requested_delivery(
    state: &TelegramAuthSnapshot,
    delivery: Option<&str>,
) -> Result<(), String> {
    let Some(requested) = delivery else {
        return Ok(());
    };
    let offered = if state.stage == "emailCode" {
        Some("email")
    } else if state.stage == "code" {
        state.next_code_type.as_deref().map(delivery_family)
    } else {
        None
    };
    if offered == Some(delivery_family(requested)) {
        Ok(())
    } else {
        Err("Telegram todavía no ofrece el canal elegido. Selecciona uno disponible en la pantalla.".into())
    }
}

#[cfg(test)]
mod delivery_choice_tests {
    use super::*;
    #[test]
    fn call_choice_enables_supported_manual_missed_call_verification() {
        let settings = phone_delivery_settings("call").unwrap().unwrap();
        assert!(settings.allow_missed_call);
        assert!(!settings.allow_sms_retriever_api);
        assert!(!settings.allow_flash_call);
        assert!(settings.firebase_authentication_settings.is_none());
    }
    #[test]
    fn other_choices_do_not_claim_to_force_a_server_channel() {
        for kind in ["sms", "email", "telegram"] {
            assert!(phone_delivery_settings(kind).unwrap().is_none());
        }
        assert!(phone_delivery_settings("unknown").is_err());
    }
    #[test]
    fn explicit_resend_cannot_silently_use_a_different_channel() {
        let mut state = TelegramAuthSnapshot {
            stage: "code".into(),
            next_code_type: Some("sms".into()),
            ..Default::default()
        };
        assert!(validate_requested_delivery(&state, Some("sms")).is_ok());
        assert!(validate_requested_delivery(&state, Some("call")).is_err());
        assert!(validate_requested_delivery(&state, Some("email")).is_err());
        state.next_code_type = Some("missedCall".into());
        assert!(validate_requested_delivery(&state, Some("call")).is_ok());
        state.next_code_type = None;
        assert!(validate_requested_delivery(&state, Some("sms")).is_err());
        state.stage = "emailCode".into();
        assert!(validate_requested_delivery(&state, Some("email")).is_ok());
        assert!(validate_requested_delivery(&state, Some("sms")).is_err());
    }
}

#[cfg(test)]
mod missed_call_instructions_tests {
    use super::*;
    #[test]
    fn missed_call_displays_required_digits_and_calling_prefix() {
        let value = serde_json::json!({"@type":"authorizationStateWaitCode", "code_info": {
            "phone_number":"15550000000",
            "type":{"@type":"authenticationCodeTypeMissedCall","length":4,"phone_number_prefix":"1555"},
            "next_type":{"@type":"authenticationCodeTypeSms","length":6}, "timeout":30
        }});
        let state: tdlib_rs::enums::AuthorizationState = serde_json::from_value(value).unwrap();
        let snapshot = snapshot_from_state(
            &serde_json::to_value(&state).unwrap(),
            &format!("{state:?}"),
        );
        assert_eq!(snapshot.code_type.as_deref(), Some("missedCall"));
        assert_eq!(
            snapshot.hint.as_deref(),
            Some("Introduce los últimos 4 dígitos del número que te llamó (empieza por 1555).")
        );
    }
    #[test]
    fn voice_call_does_not_use_next_missed_call_instructions() {
        let value = serde_json::json!({"@type":"authorizationStateWaitCode", "code_info": {
            "type":{"@type":"authenticationCodeTypeCall","length":5},
            "next_type":{"@type":"authenticationCodeTypeMissedCall","length":4,"phone_number_prefix":"1555"}
        }});
        let snapshot = snapshot_from_state(&value, "WaitCode");
        assert_eq!(
            snapshot.hint.as_deref(),
            Some("Introduce el código dictado en la llamada telefónica.")
        );
    }
}
