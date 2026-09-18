import { Cloud, X } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useEffect, useRef, useState, type FormEvent } from "react";
import {
  checkTelegramLoginEmail,
  configureTelegram,
  forgetTelegramSession,
  getTelegramAuthState,
  getTelegramLoginEmailStatus,
  logOutTelegram,
  registerTelegramUser,
  requestTelegramQr,
  resendTelegramCode,
  resendTelegramLoginEmail,
  resetTelegramAuthenticationEmail,
  resetTelegramToPhone,
  setTelegramLoginEmail,
  submitTelegramCode,
  submitTelegramEmail,
  submitTelegramEmailCode,
  submitTelegramEmailIdentity,
  submitTelegramPassword,
  submitTelegramPhone,
} from "../bridge/telegram";
import { readableError } from "../bridge/shared";
import { Dialog } from "../Dialog";
import type {
  TelegramAuthSnapshot,
  TelegramIdentityProvider,
  TelegramLoginEmailCodeInfo,
  TelegramLoginEmailStatus,
} from "../types";
import {
  AuthDeliveryOptions,
  deliveryFamily,
  type AuthDelivery,
} from "./AuthDeliveryOptions";

const COUNTRY_CALLING_CODES: Record<string, string> = {
  MX: "+52", US: "+1", CA: "+1", ES: "+34", CO: "+57", AR: "+54",
  CL: "+56", PE: "+51", EC: "+593", GT: "+502", VE: "+58", BO: "+591",
  PY: "+595", UY: "+598", CR: "+506", PA: "+507", SV: "+503", HN: "+504",
  NI: "+505", DO: "+1", PR: "+1", BR: "+55", GB: "+44", FR: "+33",
  DE: "+49", IT: "+39", PT: "+351", RU: "+7", UA: "+380", IN: "+91",
  CN: "+86", JP: "+81", KR: "+82", AU: "+61", NZ: "+64",
};

const TIMEZONE_TO_CALLING_CODE: Record<string, string> = {
  "america/mexico_city": "+52", "america/cancun": "+52", "america/merida": "+52",
  "america/monterrey": "+52", "america/mazatlan": "+52", "america/chihuahua": "+52",
  "america/hermosillo": "+52", "america/tijuana": "+52", "america/matamoros": "+52",
  "america/bahia_banderas": "+52", "america/ojinaga": "+52",
  "america/bogota": "+57",
  "america/buenos_aires": "+54", "america/argentina/buenos_aires": "+54", "america/cordoba": "+54",
  "europe/madrid": "+34", "atlantic/canary": "+34", "africa/ceuta": "+34",
  "america/lima": "+51",
  "america/santiago": "+56", "pacific/easter": "+56",
  "america/guayaquil": "+593", "pacific/galapagos": "+593",
  "america/guatemala": "+502",
  "america/caracas": "+58",
  "america/la_paz": "+591",
  "america/asuncion": "+595",
  "america/montevideo": "+598",
  "america/costa_rica": "+506",
  "america/panama": "+507",
  "america/el_salvador": "+503",
  "america/tegucigalpa": "+504",
  "america/managua": "+505",
  "america/santo_domingo": "+1",
  "america/puerto_rico": "+1",
  "america/sao_paulo": "+55",
  "europe/london": "+44",
  "europe/paris": "+33",
  "europe/berlin": "+49",
  "europe/rome": "+39",
};

export function detectCountryCallingCode(): string {
  try {
    const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone?.toLowerCase() || "";
    if (timezone && TIMEZONE_TO_CALLING_CODE[timezone]) {
      return TIMEZONE_TO_CALLING_CODE[timezone];
    }
    if (
      timezone.includes("mexico")
      || timezone.includes("monterrey")
      || timezone.includes("cancun")
      || timezone.includes("tijuana")
    ) {
      return "+52";
    }
    if (timezone.startsWith("america/argentina")) return "+54";
    if (
      timezone.startsWith("america/new_york")
      || timezone.startsWith("america/chicago")
      || timezone.startsWith("america/denver")
      || timezone.startsWith("america/los_angeles")
    ) {
      return "+1";
    }

    const locales = navigator.languages?.length
      ? navigator.languages
      : [navigator.language || ""];
    for (const locale of locales) {
      if (!locale) continue;
      const parts = locale.split(/[-_]/);
      if (parts.length >= 2) {
        const country = parts[1].toUpperCase();
        if (COUNTRY_CALLING_CODES[country]) {
          return COUNTRY_CALLING_CODES[country];
        }
      }
    }
  } catch {
    // Use the Mexico fallback below when locale APIs are unavailable.
  }
  return "+52";
}

type TelegramConnectModalProps = {
  rememberDefault: boolean;
  onClose: () => void;
  onChanged: (snapshot: TelegramAuthSnapshot) => void | Promise<void>;
};

function trustedFragmentUrl(value?: string | null): string | null {
  if (!value) return null;
  try {
    const url = new URL(value);
    const host = url.hostname.toLowerCase();
    if (url.protocol !== "https:" || host !== "fragment.com") {
      return null;
    }
    return url.toString();
  } catch {
    return null;
  }
}

function channelName(kind?: string | null): string {
  return ({
    telegram: "mensaje de Telegram",
    sms: "SMS al teléfono",
    call: "llamada telefónica",
    missedCall: "llamada perdida",
    flashCall: "llamada de verificación",
    smsWord: "palabra por SMS",
    smsPhrase: "frase por SMS",
    fragment: "Fragment",
  } as Record<string, string>)[kind ?? ""] ?? "canal indicado por Telegram";
}

export function TelegramConnectModal({
  rememberDefault,
  onClose,
  onChanged,
}: TelegramConnectModalProps) {
  const [snapshot, setSnapshot] = useState<TelegramAuthSnapshot | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [rememberSession, setRememberSession] = useState(rememberDefault);
  const [apiId, setApiId] = useState("");
  const [apiHash, setApiHash] = useState("");
  const [phone, setPhone] = useState(() => `${detectCountryCallingCode()} `);
  const [editingPhone, setEditingPhone] = useState(false);
  const [delivery, setDelivery] = useState<AuthDelivery>("sms");
  const deliveryPicked = useRef(false);
  const [email, setEmail] = useState("");
  const [identityProvider, setIdentityProvider] = useState<TelegramIdentityProvider | null>(null);
  const [identityToken, setIdentityToken] = useState("");
  const [code, setCode] = useState("");
  const [loginEmail, setLoginEmail] = useState("");
  const [loginEmailInfo, setLoginEmailInfo] = useState<TelegramLoginEmailCodeInfo | null>(null);
  const [loginEmailStatus, setLoginEmailStatus] = useState<TelegramLoginEmailStatus | null | undefined>(undefined);
  const [loginEmailCode, setLoginEmailCode] = useState("");
  const [password, setPassword] = useState("");
  const [firstName, setFirstName] = useState("");
  const [lastName, setLastName] = useState("");
  const [deadline, setDeadline] = useState(0);
  const [emailResetDeadline, setEmailResetDeadline] = useState(0);
  const [now, setNow] = useState(Date.now());
  const request = useRef(0);
  const working = useRef(false);
  const mounted = useRef(true);
  const previousConnected = useRef<boolean | null>(null);

  const apply = (next: TelegramAuthSnapshot) => {
    setSnapshot(next);
    if (!deliveryPicked.current) {
      if (next.stage === "email" || next.stage === "emailCode") {
        setDelivery("email");
      } else if (next.stage === "code") {
        setDelivery(
          deliveryFamily(next.nextCodeType)
          ?? deliveryFamily(next.codeType)
          ?? "sms",
        );
      }
    }
    const appliedAt = Date.now();
    setDeadline(
      next.stage === "code"
        ? appliedAt + Math.max(0, next.timeout ?? 0) * 1000
        : 0,
    );
    setEmailResetDeadline(
      next.stage === "emailCode" && next.emailReset
        ? appliedAt + Math.max(0, next.emailReset.seconds) * 1000
        : 0,
    );
    setNow(appliedAt);
    const wasConnected = previousConnected.current;
    previousConnected.current = next.connected;
    if (next.connected && wasConnected === false) {
      void Promise.resolve(onChanged(next)).catch((value) => setError(readableError(value)));
    }
  };

  const refresh = async () => {
    if (working.current) return;
    const id = ++request.current;
    try {
      const next = await getTelegramAuthState();
      if (mounted.current && id === request.current && !working.current) apply(next);
    } catch (value) {
      if (mounted.current && id === request.current) setError(readableError(value));
    }
  };

  const loadLoginEmailStatus = async () => {
    try {
      const status = await getTelegramLoginEmailStatus();
      if (mounted.current) setLoginEmailStatus(status);
    } catch (value) {
      if (mounted.current) {
        setLoginEmailStatus(null);
        setError(readableError(value));
      }
    }
  };

  useEffect(() => {
    mounted.current = true;
    void refresh();
    return () => {
      mounted.current = false;
      ++request.current;
    };
  }, []);

  useEffect(() => {
    if (!snapshot || !["initializing", "qr", "loggingOut", "closing"].includes(snapshot.stage)) {
      return;
    }
    const timer = window.setInterval(() => void refresh(), 1400);
    return () => window.clearInterval(timer);
  }, [snapshot?.stage]);

  useEffect(() => {
    if (!snapshot?.connected) {
      setLoginEmailStatus(undefined);
      return;
    }
    setLoginEmailStatus(undefined);
    void loadLoginEmailStatus();
  }, [snapshot?.connected]);

  useEffect(() => {
    if (!deadline && !emailResetDeadline) return;
    const tick = () => setNow(Date.now());
    const timer = window.setInterval(tick, 250);
    document.addEventListener("visibilitychange", tick);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", tick);
    };
  }, [deadline, emailResetDeadline]);

  const run = async (
    operation: () => Promise<TelegramAuthSnapshot>,
    success?: () => void,
  ) => {
    if (working.current) return;
    working.current = true;
    ++request.current;
    setBusy(true);
    setError(null);
    try {
      const next = await operation();
      if (mounted.current) {
        apply(next);
        success?.();
      }
    } catch (value) {
      if (mounted.current) setError(readableError(value));
    } finally {
      working.current = false;
      if (mounted.current) setBusy(false);
    }
  };

  const runLoginEmailInfo = async (
    operation: () => Promise<TelegramLoginEmailCodeInfo>,
  ) => {
    if (working.current) return;
    working.current = true;
    ++request.current;
    setBusy(true);
    setError(null);
    try {
      const info = await operation();
      if (mounted.current) {
        setLoginEmailInfo(info);
        setLoginEmailCode("");
      }
    } catch (value) {
      if (mounted.current) setError(readableError(value));
    } finally {
      working.current = false;
      if (mounted.current) setBusy(false);
    }
  };

  const submitIdentity = (provider: TelegramIdentityProvider) => {
    if (!identityToken.trim()) {
      setError("Introduce el ID token OAuth emitido para Nuvio.");
      return;
    }
    void run(
      () => submitTelegramEmailIdentity(provider, identityToken.trim()),
      () => {
        setIdentityToken("");
        setIdentityProvider(null);
      },
    );
  };

  const openFragment = async () => {
    const url = trustedFragmentUrl(snapshot?.fragmentUrl);
    if (!url) {
      setError("Telegram no proporcionó una URL HTTPS válida de Fragment para este intento.");
      return;
    }
    try {
      await openUrl(url);
    } catch (value) {
      setError(readableError(value));
    }
  };

  const stage = snapshot?.stage;
  const remaining = Math.max(0, Math.ceil((deadline - now) / 1000));
  const emailResetRemaining = Math.max(
    0,
    Math.ceil((emailResetDeadline - now) / 1000),
  );
  const nextType = snapshot?.nextCodeType;
  const canResend =
    stage === "emailCode"
    || (stage === "code" && !!nextType && nextType !== "none");

  const sendPhone = () => {
    const normalized = phone.replace(/\s+/g, "");
    if (!/^\+[0-9]{7,15}$/.test(normalized)) {
      setError("Introduce tu teléfono con +, código de país y entre 7 y 15 dígitos.");
      return;
    }
    void run(
      () => submitTelegramPhone(normalized, delivery),
      () => {
        setEditingPhone(false);
        setCode("");
      },
    );
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (working.current) return;
    if (editingPhone || stage === "phone") {
      sendPhone();
      return;
    }

    switch (stage) {
      case "needsCredentials": {
        const id = Number(apiId.trim());
        if (
          !Number.isInteger(id)
          || id <= 0
          || id > 2147483647
          || !/^[a-fA-F0-9]{32}$/.test(apiHash.trim())
        ) {
          setError("API ID debe estar entre 1 y 2147483647 y API Hash debe tener 32 caracteres hexadecimales.");
          return;
        }
        void run(
          () => configureTelegram(id, apiHash.trim(), rememberSession),
          () => setApiHash(""),
        );
        break;
      }
      case "email":
        void run(() => submitTelegramEmail(email.trim()));
        break;
      case "emailCode":
        void run(() => submitTelegramEmailCode(code), () => setCode(""));
        break;
      case "code":
        void run(() => submitTelegramCode(code), () => setCode(""));
        break;
      case "password":
        void run(() => submitTelegramPassword(password), () => setPassword(""));
        break;
      case "registration":
        void run(() => registerTelegramUser(firstName, lastName));
        break;
      default:
        void refresh();
    }
  };

  return (
    <Dialog className="connect-modal" labelledBy="connect-title" onClose={onClose}>
      <button className="icon-button modal-close" onClick={onClose} aria-label="Cerrar">
        <X size={18} />
      </button>
      <div className="modal-brand"><Cloud size={24} /></div>
      <h2 id="connect-title">
        {snapshot?.connected ? "Telegram conectado" : "Conectar Telegram"}
      </h2>
      <p>{snapshot?.message ?? "Consultando la conexión de Telegram…"}</p>
      {error && <div className="modal-error" role="alert">{error}</div>}

      {snapshot?.connected ? (
        <div className="auth-success account-connected-panel">
          <strong>{snapshot.accountLabel ?? "Cuenta de Telegram"}</strong>
          <span>{snapshot.isPremium ? "Telegram Premium" : "Cuenta estándar"}</span>
          <div className="auth-login-email-panel" data-testid="login-email-setup">
            <strong>Correo para futuros accesos</strong>
            <span>
              Telegram puede usar un correo de login para reducir la dependencia de otra sesión.
              Nuvio solo permite configurarlo cuando TDLib confirma que la cuenta es elegible.
            </span>
            {loginEmailStatus === undefined ? (
              <span>Consultando disponibilidad con Telegram…</span>
            ) : loginEmailStatus === null ? (
              <span data-testid="login-email-status-error">
                No se pudo consultar la disponibilidad del correo de acceso.
              </span>
            ) : loginEmailInfo ? (
              <>
                <span>
                  Código enviado a {loginEmailInfo.emailPattern || "tu correo"}
                  {loginEmailInfo.codeLength > 0 ? ` · ${loginEmailInfo.codeLength} caracteres` : ""}
                </span>
                <input
                  aria-label="Código del correo de login"
                  inputMode="numeric"
                  value={loginEmailCode}
                  onChange={(event) => setLoginEmailCode(event.target.value)}
                  autoComplete="one-time-code"
                  maxLength={loginEmailInfo.codeLength > 0 ? loginEmailInfo.codeLength : undefined}
                />
                <div className="auth-inline-actions">
                  <button
                    type="button"
                    className="secondary-button"
                    disabled={busy || !loginEmailCode.trim()}
                    onClick={() => void run(
                      () => checkTelegramLoginEmail(loginEmailCode),
                      () => {
                        setLoginEmailInfo(null);
                        setLoginEmailCode("");
                        setLoginEmail("");
                        void loadLoginEmailStatus();
                      },
                    )}
                  >
                    Verificar correo
                  </button>
                  <button
                    type="button"
                    className="auth-link-button"
                    disabled={busy}
                    onClick={() => void runLoginEmailInfo(resendTelegramLoginEmail)}
                  >
                    Reenviar código
                  </button>
                </div>
              </>
            ) : loginEmailStatus.available ? (
              <>
                {loginEmailStatus.emailPattern && (
                  <span data-testid="login-email-current">
                    Correo actual: {loginEmailStatus.emailPattern}
                  </span>
                )}
                {loginEmailStatus.required && (
                  <span data-testid="login-email-required">
                    Telegram solicita configurar un correo de acceso para esta cuenta.
                  </span>
                )}
                <input
                  aria-label="Correo para futuros accesos"
                  type="email"
                  value={loginEmail}
                  onChange={(event) => setLoginEmail(event.target.value)}
                  placeholder="correo@ejemplo.com"
                  autoComplete="email"
                />
                <button
                  type="button"
                  className="secondary-button"
                  disabled={busy || !loginEmail.trim()}
                  onClick={() => void runLoginEmailInfo(
                    () => setTelegramLoginEmail(loginEmail.trim()),
                  )}
                >
                  {loginEmailStatus.emailPattern ? "Cambiar correo de acceso" : "Configurar correo de acceso"}
                </button>
              </>
            ) : (
              <span data-testid="login-email-unavailable">
                Telegram no habilitó la configuración de correo de acceso para esta cuenta.
              </span>
            )}
          </div>
          <div className="account-actions">
            <button
              disabled={busy}
              className="secondary-button"
              onClick={() => void run(logOutTelegram)}
            >
              Cerrar sesión
            </button>
            <button
              disabled={busy}
              className="secondary-button"
              onClick={() => void run(forgetTelegramSession)}
            >
              Olvidar sesión
            </button>
          </div>
        </div>
      ) : (
        <form className="credential-placeholder" onSubmit={submit}>
          {snapshot
            && stage
            && ["phone", "email", "emailCode", "code"].includes(stage)
            && !editingPhone
            && (
              <AuthDeliveryOptions
                selected={delivery}
                snapshot={snapshot}
                remaining={remaining}
                busy={busy}
                onSelect={(method) => {
                  deliveryPicked.current = true;
                  setDelivery(method);
                  setError(null);
                }}
              />
            )}

          {stage === "needsCredentials" && (
            <>
              <label htmlFor="telegram-api-id">API ID</label>
              <input
                id="telegram-api-id"
                inputMode="numeric"
                value={apiId}
                onChange={(event) => setApiId(event.target.value)}
                autoComplete="off"
                required
              />
              <label htmlFor="telegram-api-hash">API Hash</label>
              <input
                id="telegram-api-hash"
                type="password"
                value={apiHash}
                onChange={(event) => setApiHash(event.target.value)}
                autoComplete="off"
                required
              />
              <label className="remember-session-control">
                <input
                  type="checkbox"
                  checked={rememberSession}
                  onChange={(event) => setRememberSession(event.target.checked)}
                />
                <span>Recordar sesión en este equipo</span>
              </label>
            </>
          )}

          {(stage === "phone" || editingPhone) && (
            <>
              <label htmlFor="telegram-phone">Teléfono con código de país</label>
              <input
                id="telegram-phone"
                type="tel"
                value={phone}
                onChange={(event) => setPhone(event.target.value)}
                autoComplete="tel"
                required
                autoFocus
              />
              {snapshot && snapshot.futureAuthTokenCount > 0 && !editingPhone && (
                <span className="auth-hint" data-testid="future-auth-ready">
                  Acceso automático preparado con {snapshot.futureAuthTokenCount}
                  {" "}token{snapshot.futureAuthTokenCount === 1 ? "" : "s"} cifrado{snapshot.futureAuthTokenCount === 1 ? "" : "s"}.
                  Telegram intentará reutilizarlo antes de pedir otro código.
                </span>
              )}
            </>
          )}

          {stage === "email" && !editingPhone && (
            <>
              <label htmlFor="telegram-email">
                Correo de autenticación solicitado por Telegram
              </label>
              <input
                id="telegram-email"
                type="email"
                value={email}
                onChange={(event) => setEmail(event.target.value)}
                autoComplete="email"
                required
                autoFocus
              />
            </>
          )}

          {snapshot
            && !editingPhone
            && (stage === "email" || stage === "emailCode")
            && (snapshot.allowGoogleId || snapshot.allowAppleId)
            && (
              <div className="auth-identity-options" data-testid="identity-options">
                <span className="auth-delivery-label">Identidad vinculada al correo</span>
                <p className="auth-hint">
                  Telegram habilitó identidad federada para este paso. Nuvio valida el ID token
                  directamente con TDLib; el token debe haber sido emitido por Google o Apple para Nuvio.
                </p>
                <div className="auth-inline-actions">
                  {snapshot.allowGoogleId && (
                    <button
                      type="button"
                      className={`secondary-button ${identityProvider === "google" ? "selected" : ""}`}
                      disabled={busy}
                      onClick={() => {
                        setIdentityProvider("google");
                        setIdentityToken("");
                        setError(null);
                      }}
                    >
                      Google ID
                    </button>
                  )}
                  {snapshot.allowAppleId && (
                    <button
                      type="button"
                      className={`secondary-button ${identityProvider === "apple" ? "selected" : ""}`}
                      disabled={busy}
                      onClick={() => {
                        setIdentityProvider("apple");
                        setIdentityToken("");
                        setError(null);
                      }}
                    >
                      Apple ID
                    </button>
                  )}
                </div>
                {identityProvider && (
                  <>
                    <label htmlFor="telegram-identity-token">
                      ID token de {identityProvider === "google" ? "Google" : "Apple"}
                    </label>
                    <input
                      id="telegram-identity-token"
                      type="password"
                      value={identityToken}
                      onChange={(event) => setIdentityToken(event.target.value)}
                      autoComplete="off"
                      placeholder="Token OAuth emitido para Nuvio"
                    />
                    <button
                      type="button"
                      className="secondary-button"
                      disabled={busy || !identityToken.trim()}
                      onClick={() => submitIdentity(identityProvider)}
                    >
                      Continuar con {identityProvider === "google" ? "Google ID" : "Apple ID"}
                    </button>
                  </>
                )}
              </div>
            )}

          {(stage === "code" || stage === "emailCode") && !editingPhone && (
            <>
              <label htmlFor="telegram-code">
                {stage === "emailCode"
                  ? "Código de correo electrónico"
                  : `Código por ${channelName(snapshot?.codeType)}`}
              </label>
              {snapshot?.hint && <span className="auth-hint">{snapshot.hint}</span>}
              <input
                id="telegram-code"
                inputMode={
                  snapshot?.codeType === "smsWord" || snapshot?.codeType === "smsPhrase"
                    ? "text"
                    : "numeric"
                }
                value={code}
                onChange={(event) => setCode(event.target.value)}
                autoComplete="one-time-code"
                maxLength={
                  stage === "emailCode"
                    ? snapshot?.emailCodeLength ?? undefined
                    : snapshot?.codeLength ?? undefined
                }
                required
                autoFocus
              />
            </>
          )}

          {stage === "code"
            && snapshot?.codeType === "fragment"
            && snapshot.fragmentUrl
            && !editingPhone
            && (
              <div className="fragment-auth-panel" data-testid="fragment-auth">
                <strong>Obtener código en Fragment</strong>
                <span>
                  Telegram proporcionó una URL oficial de Fragment para este intento.
                  Nuvio abrirá esa dirección en tu navegador; vuelve aquí con el código.
                </span>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={busy}
                  onClick={() => void openFragment()}
                >
                  Abrir Fragment
                </button>
              </div>
            )}

          {stage === "emailCode"
            && snapshot?.emailReset
            && !editingPhone
            && (
              <div className="auth-email-reset" data-testid="email-reset">
                <span className="auth-hint">
                  {snapshot.emailReset.state === "pending"
                    ? emailResetRemaining > 0
                      ? `Restablecimiento de correo solicitado · faltan ${emailResetRemaining}s.`
                      : "El periodo de espera terminó. Puedes comprobar el restablecimiento."
                    : emailResetRemaining > 0
                      ? `Telegram permitirá restablecer este correo en ${emailResetRemaining}s.`
                      : "Telegram permite restablecer el correo y volver a un método de teléfono."}
                </span>
                <button
                  type="button"
                  className="secondary-button"
                  disabled={busy || emailResetRemaining > 0}
                  onClick={() => void run(resetTelegramAuthenticationEmail)}
                >
                  {snapshot.emailReset.state === "pending"
                    ? "Comprobar restablecimiento"
                    : "Restablecer correo de autenticación"}
                </button>
              </div>
            )}

          {stage === "password" && !editingPhone && (
            <>
              <label htmlFor="telegram-password">
                Contraseña de verificación en dos pasos
              </label>
              {snapshot?.hint && <span className="auth-hint">{snapshot.hint}</span>}
              <input
                id="telegram-password"
                type="password"
                value={password}
                onChange={(event) => setPassword(event.target.value)}
                autoComplete="current-password"
                required
              />
            </>
          )}

          {stage === "registration" && (
            <>
              <label htmlFor="telegram-first-name">Nombre</label>
              <input
                id="telegram-first-name"
                value={firstName}
                onChange={(event) => setFirstName(event.target.value)}
                required
              />
              <label htmlFor="telegram-last-name">Apellido</label>
              <input
                id="telegram-last-name"
                value={lastName}
                onChange={(event) => setLastName(event.target.value)}
              />
            </>
          )}

          {stage === "qr" && (
            <div className="telegram-qr-panel">
              {snapshot?.qrSvg && (
                <div
                  className="telegram-qr"
                  dangerouslySetInnerHTML={{ __html: snapshot.qrSvg }}
                />
              )}
              <p>
                Abre Telegram en tu otro dispositivo: Ajustes → Dispositivos → Vincular
                dispositivo.
              </p>
            </div>
          )}

          {stage
            && ["needsCredentials", "phone", "email", "code", "emailCode", "password", "registration"].includes(stage)
            && (
              <button className="primary-button modal-primary" disabled={busy} type="submit">
                {busy
                  ? "Procesando…"
                  : editingPhone || stage === "phone"
                    ? "Continuar con mi número"
                    : stage === "email"
                      ? "Enviar código al correo"
                      : stage === "needsCredentials"
                        ? "Continuar con Telegram"
                        : "Continuar"}
              </button>
            )}

          {canResend && !editingPhone && (
            <button
              type="button"
              className="secondary-button"
              disabled={busy || remaining > 0}
              onClick={() => void run(
                () => resendTelegramCode(
                  stage === "emailCode" ? "email" : nextType ?? undefined,
                ),
                () => {
                  setCode("");
                  setDelivery(
                    stage === "emailCode"
                      ? "email"
                      : deliveryFamily(nextType) ?? delivery,
                  );
                },
              )}
            >
              {remaining > 0
                ? `Esperar ${remaining}s para reenviar`
                : stage === "emailCode"
                  ? "Reenviar código al correo"
                  : `Solicitar código por ${channelName(nextType)}`}
            </button>
          )}

          {stage === "code" && !canResend && (
            <span className="auth-hint">
              Telegram no ofrece un canal de reenvío en este momento.
            </span>
          )}

          {stage
            && ["code", "email", "emailCode", "password"].includes(stage)
            && (
              <button
                type="button"
                className="auth-link-button"
                disabled={busy}
                onClick={() => {
                  setEditingPhone(!editingPhone);
                  setError(null);
                }}
              >
                {editingPhone
                  ? "Cancelar cambio de número"
                  : "Corregir número de teléfono"}
              </button>
            )}

          {stage
            && ["phone", "email", "emailCode", "code", "password", "registration"].includes(stage)
            && !editingPhone
            && (
              <button
                type="button"
                className="auth-link-button"
                disabled={busy}
                onClick={() => void run(requestTelegramQr)}
              >
                Vincular con código QR
              </button>
            )}

          {stage === "qr" && (
            <button
              type="button"
              className="auth-link-button"
              disabled={busy}
              onClick={() => void run(resetTelegramToPhone)}
            >
              Volver al teléfono
            </button>
          )}

          {stage === "closed" && (
            <p>Cierra y vuelve a abrir Nuvio para iniciar otra sesión.</p>
          )}
        </form>
      )}
    </Dialog>
  );
}
