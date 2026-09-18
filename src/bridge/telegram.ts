import { invoke } from "@tauri-apps/api/core";
import type { TelegramAuthSnapshot } from "../types";

export function getTelegramAuthState(): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_auth_state");
}

export function configureTelegram(
  apiId: number,
  apiHash: string,
  rememberSession: boolean,
): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_configure", {
    apiId,
    apiHash,
    rememberSession,
  });
}

export function submitTelegramPhone(
  phone: string,
  delivery?: string,
): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_submit_phone", { phone, delivery });
}

export function submitTelegramEmail(email: string): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_submit_email", { email });
}

export function submitTelegramEmailCode(code: string): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_submit_email_code", { code });
}

export function submitTelegramCode(code: string): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_submit_code", { code });
}

export function resendTelegramCode(delivery?: string): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_resend_code", { delivery });
}

export function submitTelegramPassword(password: string): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_submit_password", { password });
}

export function requestTelegramQr(): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_request_qr");
}

export function registerTelegramUser(
  firstName: string,
  lastName: string,
): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_register_user", { firstName, lastName });
}

export function logOutTelegram(): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_log_out");
}

export function forgetTelegramSession(): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_forget_session");
}

export function resetTelegramToPhone(): Promise<TelegramAuthSnapshot> {
  return invoke<TelegramAuthSnapshot>("telegram_reset_to_phone");
}
