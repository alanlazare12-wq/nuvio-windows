import { Mail, MessageCircle, Phone, Smartphone } from "lucide-react";
import type { TelegramAuthSnapshot } from "../types";
import "./AuthDeliveryOptions.css";

export type AuthDelivery = "telegram" | "sms" | "email" | "call";
const methods = [
  { id: "sms", title: "SMS al teléfono", description: "En tu app de mensajes", icon: Smartphone },
  { id: "email", title: "Correo electrónico", description: "En tu bandeja de entrada", icon: Mail },
  { id: "call", title: "Llamada telefónica", description: "En el número que indiques", icon: Phone },
  { id: "telegram", title: "Mensaje de Telegram", description: "En otra sesión de Telegram", icon: MessageCircle },
] as const;

export function deliveryFamily(kind?: string | null): AuthDelivery | null {
  if (["sms", "smsWord", "smsPhrase"].includes(kind ?? "")) return "sms";
  if (["call", "missedCall", "flashCall"].includes(kind ?? "")) return "call";
  return kind === "telegram" || kind === "email" ? kind : null;
}

export function AuthDeliveryOptions({ selected, snapshot, remaining, busy, onSelect }: {
  selected: AuthDelivery; snapshot: TelegramAuthSnapshot; remaining: number; busy: boolean;
  onSelect: (method: AuthDelivery) => void;
}) {
  const current = snapshot.stage === "email" || snapshot.stage === "emailCode" ? "email" : snapshot.stage === "code" ? deliveryFamily(snapshot.codeType) : null;
  const next = snapshot.stage === "code" ? deliveryFamily(snapshot.nextCodeType) : null;
  const beforePhone = snapshot.stage === "phone";
  const status = (id: AuthDelivery) => {
    if (beforePhone) return id === selected ? "Seleccionado" : "Elegir";
    if (id === current) return snapshot.stage === "email" ? "Disponible" : "Código enviado";
    if (id === next) return remaining > 0 ? `Disponible en ${remaining}s` : "Disponible para reenviar";
    return "No ofrecido ahora";
  };
  let explanation: string;
  if (beforePhone) {
    explanation = selected === "email"
      ? "Primero confirma tu teléfono. Si Telegram habilita el correo para tu cuenta, podrás introducir aquí la dirección."
      : selected === "sms" ? "El SMS llega a la mensajería de tu teléfono. Primero confirma tu número para consultar si Telegram habilita este envío."
      : selected === "call" ? "Solicitaremos la verificación de tu número permitiendo llamadas. Telegram confirma si puede ofrecer este canal."
      : "El código llegará a una sesión de Telegram que ya tengas abierta, si ese canal está disponible.";
  } else if (selected === current) {
    explanation = selected === "email" && snapshot.stage === "email"
      ? "Telegram habilitó el correo. Introduce tu dirección para recibir el código."
      : selected === "sms" ? "El código actual se envió por SMS. Revisa la app de mensajes de tu teléfono."
      : selected === "email" ? "El código actual se envió a tu correo. Revisa también la carpeta de spam."
      : selected === "call" ? "El código actual se envió mediante llamada. Sigue las indicaciones que aparecen debajo."
      : "El código actual se envió como mensaje de Telegram. Puedes elegir otro canal disponible arriba.";
  } else if (selected === next) {
    explanation = remaining > 0
      ? `Tu elección está disponible en ${remaining}s. Al terminar la espera podrás solicitar el envío con el botón de abajo.`
      : "Tu elección está disponible. Pulsa el botón de envío de abajo para recibir un nuevo código por este canal.";
  } else {
    explanation = selected === "email"
      ? "Telegram no habilitó el correo en este intento. La dirección se puede introducir cuando Telegram ofrece ese paso."
      : `Telegram no ofrece ${selected === "sms" ? "SMS" : selected === "call" ? "llamada" : "mensaje en la app"} en este intento. Puedes elegir otro canal que figure como disponible.`;
  }
  return <div className="auth-delivery">
    <span className="auth-delivery-label" id="auth-delivery-label">¿Dónde quieres recibir el código?</span>
    <div className="auth-delivery-grid" role="group" aria-labelledby="auth-delivery-label">
      {methods.map(({id, title, description, icon: Icon}) => <button key={id} type="button"
        className={`auth-delivery-option ${selected === id ? "selected" : ""}`}
        aria-pressed={selected === id} aria-label={title} disabled={busy}
        onClick={() => onSelect(id)}>
        <Icon size={20} aria-hidden="true" />
        <strong>{title}</strong><span>{description}</span><small>{status(id)}</small>
      </button>)}
    </div>
    <p className="auth-delivery-explanation" role="status" aria-live="polite">{explanation}</p>
  </div>;
}
