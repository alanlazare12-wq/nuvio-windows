# Nuvio 1.2.2 — verificación y entrega

Fecha: 12 de septiembre de 2026.

Se corrigió el diálogo de autenticación en Android y Windows. Ahora muestra cuatro botones desde el paso del teléfono: SMS al teléfono, correo electrónico, llamada telefónica y mensaje de Telegram. La elección se conserva durante la solicitud y el diálogo diferencia el canal actual del siguiente canal de reenvío.

La elección de llamada permite la verificación manual por llamada perdida. Cuando Telegram devuelve el prefijo y la longitud del código, ambos se muestran en las instrucciones. El backend rechaza un reenvío solicitado para un canal diferente del que Telegram ofrece.

Los canales disponibles los determina Telegram. TDLib no permite forzar SMS, correo o voz para una cuenta que no los recibe. El correo se introduce únicamente cuando el servidor llega al estado que lo solicita. Elegir una tarjeta no envía nada por sí solo: se confirma el teléfono y, si hay reenvío, se pulsa la acción que nombra el canal de destino.

## Validación

| Proyecto | Rust | Autenticación UI | Interfaz general |
| --- | ---: | ---: | ---: |
| Nuvio-Android | 72 | 20 | 26 |
| Nuvio-Windows | 61 | 20 | 24 |

Todas las pruebas pasaron. TypeScript y Clippy con -D warnings también pasaron en ambos proyectos. Las pruebas Rust se ejecutaron en el host Windows; el APK se compiló para aarch64-linux-android.

Las pruebas de autenticación usan la interfaz real con IPC simulado, sin enviar mensajes reales ni iniciar sesión en una cuenta. Cubren las cuatro elecciones, el paso de correo, reenvío por SMS, cuenta regresiva, códigos incorrectos, reintento, contraseña de segundo factor, QR y retorno al teléfono. Las pruebas de diseño cubren 412×915, 412×500, 915×412 y 1280×820. Se inspeccionaron las capturas de móvil y escritorio. No había un S24 Ultra físico conectado.

APK: firma y certificado existentes verificados, ARM64, versión 1.2.2 (1002002), aplicación de producción no depurable, ELF y ZIP alineados para páginas de 16 KB. AAB: firma verificada con jarsigner y contenido nativo comprobado. El instalador Windows se comparó por SHA-256 con la salida NSIS 1.2.2. El AAB está firmado para subir a Play Console; la revisión y aprobación de Google Play ocurren en ese servicio.

Graphify actualizado y verificado por proyecto: Android 875 nodos/2176 relaciones, Windows 828 nodos/2074 relaciones. Context7 consultado para las APIs de TDLib de teléfono, correo, llamada perdida y reenvío.

Las correcciones previas de arranque y notificaciones de Android siguen incluidas. El servicio nativo de notificaciones se probó previamente en Android 16 API 36: progreso en segundo plano tras Home, tiempo restante, sincronización junto con subida, permisos denegados y finalización/timeout. Esta corrección de autenticación no modifica ese servicio.

## Archivos entregados

- APK Android ARM64 1.2.2: `Nuvio-Android-1.2.2-arm64.apk` — 58326907 bytes. SHA-256: `8b745f26433e004f0d325ad7b1b8b027dc6aa1afe841e7477a40e8d6ba13e28b`.
- AAB para Google Play 1.2.2: `Nuvio-GooglePlay-1.2.2-arm64.aab` — 24584444 bytes. SHA-256: `36ce0272bb3dd413bf390aa0bee58238ad12d0f9db11051624d4337c4734f7b5`.
- Instalador Windows x64 1.2.2: `Nuvio-Windows-1.2.2-x64.exe` — 231160934 bytes. SHA-256: `8c74f33e2d5ebbbc314e93000734ce81a9a51573b17af3018e778d0db046f2b5`.

Documentación consultada: [settings de teléfono](https://core.telegram.org/tdlib/docs/classtd_1_1td__api_1_1phone_number_authentication_settings.html), [reenvío de autenticación](https://core.telegram.org/tdlib/docs/classtd_1_1td__api_1_1resend_authentication_code.html), [llamada perdida](https://core.telegram.org/tdlib/docs/classtd_1_1td__api_1_1authentication_code_type_missed_call.html).
