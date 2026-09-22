# Historial de Nuvio

> Archivo de continuidad del proyecto. A partir del ciclo 1.2.3-dev se actualiza durante el desarrollo cada vez que se incorpora o corrige una funcionalidad relevante, antes de dar el trabajo por terminado. Cada entrada conserva decisiones técnicas, QA y pendientes para poder retomar el proyecto sin depender del historial del chat.

## 1.2.7-dev — 2026-09-21 — Subidas grandes en serie y vigilancia por progreso

### Transferencias

- Diagnosticado el fallo de los respaldos divididos: un cliente TDLib reparte un único presupuesto de subida entre todos los archivos que envía a la vez, así que cuatro volúmenes de ~2 GB en paralelo avanzaban unos pocos MB por turno y ninguno llegaba a confirmarse.
- Las subidas de 128 MB o más (`SERIAL_UPLOAD_MIN_BYTES`) pasan por un cupo serial de un permiso. Mientras una ocupa el canal, el worker sólo reclama transferencias por debajo de ese umbral (`claim_pending_under`); los demás volúmenes esperan en SQLite como `ready` en vez de ocupar un worker sin mover bytes. La concurrencia configurable sigue aplicando a los archivos pequeños.
- El worker reclama primero las subidas que ya tienen mensaje en Telegram: TDLib sigue empujando sus bytes aunque Nuvio no las esté observando, así que reanudar una de ellas siempre es mejor que abrir una segunda subida que competiría por el mismo presupuesto. Las descargas siempre llevan `message_id`, por lo que conservan el orden de cola.
- Sustituido el plazo fijo de una hora por transferencia —que mataba cualquier volumen grande en enlaces lentos aunque estuviera avanzando— por un vigilante de progreso: 20 min sin mover bytes, 30 min sin que Telegram confirme un archivo ya recibido completo, o un tope absoluto de 12 h. La muestra se compara por desigualdad porque TDLib reinicia una subida interrumpida desde un offset menor y reenviar esos bytes es progreso, no un bloqueo. Mismo vigilante para las descargas, que tenían el mismo plazo fijo.
- El checkpoint del catálogo se sube con `preliminaryUploadFile` y prioridad explícita 32, y cancela su subida preliminar si el `sendMessage` falla, para que un snapshot pequeño no quede en 0 bytes detrás de las subidas del usuario ni acumule trabajo oculto en cada reintento.
- El selector "Subidas simultáneas" aclara que aplica a los archivos pequeños, para que el ajuste no contradiga lo que el usuario ve con volúmenes grandes.

### Subidas

- El aviso "Esperando progreso de Telegram" saltaba tras sólo 8 s sin bytes nuevos y borraba la velocidad y el ETA válidos de una subida que avanzaba con normalidad. TDLib informa el progreso por partes completas: en un enlace lento dos muestras ya están a 5.5 s (4 MB a 750 KB/s) y el jitter normal superaba el umbral constantemente. Ahora espera 45 s y, mientras tanto, la interfaz conserva la última velocidad y estimación persistidas.
- Añadida la cancelación de subidas huérfanas al arrancar. Un cierre inesperado deja las transferencias en curso como `paused` y los reintentos agotados las dejan en `failed`, pero en ambos casos TDLib sigue enviando sus mensajes en segundo plano: gastaban el presupuesto de subida de la cuenta detrás de la cola que el usuario sí está mirando. `discard_abandoned_uploads` las detiene una vez por sesión, respetando las que sí van a reanudarse (`retry_wait`) o están activas, y dejando intacto un mensaje que ya se envió para que la sincronización lo adopte como subida terminada.

### Conexiones con Telegram

- Ampliado el grupo de conexiones que TDLib abre a cada datacenter, el equivalente del "upload speed boost" de Kotatogram. TDLib decide ese número a partir de una opción que no documenta: `session_count`. Su código hace `is_premium = get_option_boolean("is_premium") || session_count > 1`, y de ahí salen 8 sesiones de subida en vez de 4 (en DC2/DC4) y 8 de descarga en vez de 2.
- Nuvio ahora pide `session_count = 2`. Dos y no más porque el interruptor es booleano: un número mayor no añade ni una sesión de archivos y sólo multiplica la sesión *principal*, que es la que transporta las llamadas normales de API y la que atrae FLOOD_WAIT.
- La opción se envía **antes** de `setTdlibParameters` en las tres rutas que levantan un cliente (arranque, configuración de credenciales y reinicio por QR), porque TDLib la lee una sola vez mientras construye las sesiones de cada datacenter; cambiarla después sólo redimensiona la principal.
- El resultado queda registrado como `telegram_session_count_requested` en `catalog-sync.log`, con si TDLib la aceptó, para poder comprobarlo en lugar de suponerlo. Si una versión de TDLib ignorase la opción, el cliente se queda con el grupo por defecto: el comportamiento de hoy.

### Paralelismo de subida

- Revertido el efecto colateral de la serialización. Al forzar un solo volumen grande a la vez, la velocidad total cayó a ~0.8 MB/s en un enlace de 339 Mbps. Reconstruyendo las marcas de tiempo de Telegram de un respaldo real de 89 volúmenes, esta cuenta alcanzó **14.36 MB/s** y los sostuvo durante decenas de partes, así que una sola conexión deja la mayor parte del enlace sin usar.
- Añadido el ajuste `large_upload_concurrency` (1-4, por defecto 4) que gobierna cuántas subidas de 128 MB o más comparten el presupuesto de TDLib, con su propio control en la interfaz. El valor por defecto restablece el comportamiento previo a la serialización; bajarlo a 1 hace que cada volumen termine antes de empezar el siguiente.
- Nota sobre la causa raíz del bug original: el mismo binario, el mismo día, pasó de 14.36 MB/s (09:46) a 1.2 MB/s (14:21) y de vuelta a 10 MB/s (18:34). Esa oscilación es externa al cliente. En los tramos lentos, cuatro volúmenes en paralelo dejaban a cada archivo en ~0.3 MB/s, de modo que una parte de 1.9 GB necesitaba ~85 minutos y moría contra el plazo fijo de 60. El paralelismo nunca fue el problema: lo era el reloj, ya sustituido por el vigilante de progreso.

### Compresión

- Diagnosticada la lentitud al dividir respaldos grandes: `THREAD_MODE_BACKGROUND_BEGIN` no sólo baja la prioridad de CPU, también deja el hilo en prioridad de E/S "Very Low", el nivel que Windows reserva para el indexador y el desfragmentador y estrangula en cuanto algo más toca el disco. Medido en un host NVMe, mantenía la división secuencial en **2.4 MB/s**: un respaldo de 90 GB tardaba más de diez horas sólo en la primera pasada.
- Sustituido por una reducción de prioridad exclusivamente de CPU. Todos los perfiles corren por debajo de lo normal, Máximo incluido, porque en esta carga la prioridad más baja también resultó ser **la más rápida**: en tres mediciones independientes la misma división de 4 GiB tardó 16-28 s por debajo de lo normal y 36-93 s a prioridad normal, sin que ninguna muestra se solapara. Un hilo a prioridad normal retiene la CPU entre finalizaciones de E/S, deja sin turno a los hilos de vaciado del administrador de caché y termina bloqueado en escrituras síncronas. Máximo se gana su nombre con `compression_concurrency`, no peleando contra el planificador.
- Eliminado el `sleep` de 1 ms por cada 8 MB del perfil Equilibrado: en Windows la resolución del temporizador puede convertir ese `Sleep(1)` en un tick completo de 15.6 ms. Ahora cede la rebanada de CPU (`yield`) sin dormir. El perfil Bajo conserva su pausa real, porque ahí el tope de rendimiento es el objetivo. Todos los perfiles ceden: una pasada que nunca cede resultó 4x más lenta.
- La verificación de volúmenes lee cada parte **una sola vez** y obtiene de esos mismos bytes dos SHA-256: el del `.zip` terminado y el del contenido almacenado dentro, intersectando el rango del payload con cada bloque de 1 MiB. `create_archives_with_profile` devuelve ahora `ArchiveArtifact` con ese hash verificado y la preparación de la subida lo reutiliza en vez de releer el archivo entero. De tres lecturas por volumen a dos.
- Las pasadas secuenciales largas abren con `FILE_FLAG_SEQUENTIAL_SCAN` para que el administrador de caché descarte esas páginas al consumirlas, en lugar de dejar que un respaldo de 90 GB barra la caché del sistema y expulse lo que el usuario tiene abierto.
- Resultado medido sobre 4 GiB divididos en 47 volúmenes: 15-21 s por pasada completa (escritura + verificación), ~250 MiB/s de origen procesado, frente a los 2.4 MB/s anteriores.

### QA

- Suite Rust: 112/112 aprobados, con tests nuevos para los turnos de volúmenes, la prioridad de reanudación, los umbrales del vigilante, la equivalencia entre el hash reutilizado y el del volumen en disco, y la frontera entre una subida huérfana y una que sí va a reanudarse.
- `pnpm lint:rust` (`clippy --all-targets -D warnings`), `pnpm check` y QA UI (22/22): aprobados.
- Nuevo benchmark reproducible `pnpm qa:zip-perf`: divide 4 GiB y reporta MiB/s por perfil, corriendo cada uno en ambas posiciones para descartar el sesgo de la caché.

## 1.2.5 — 2026-09-18 — Catálogo incremental, sincronización realtime y rendimiento masivo

### Arquitectura y sincronización

- SQLite ahora usa un writer único y conexiones reader independientes bajo WAL para evitar que dashboard, búsqueda y deltas compitan innecesariamente con escrituras del sync.
- Sustituido el cursor incremental basado en `rowid` por `catalog_changes(seq)` con triggers para inserts, updates, moves, trash y deletes; los cambios sobre filas existentes ya no dependen de un refresh completo final.
- El heartbeat del dashboard dejó de transportar el catálogo completo: usa `DashboardStatus`, `catalogCursor` e `historyCursor`, y sólo invalida páginas cuando realmente cambia el catálogo.
- `list_folders()` pasó de subconsultas correlacionadas por carpeta a agregaciones por `GROUP BY`; settings se leen en una sola consulta y la cola usa agregados SQL.
- Worker de transferencias migrado a `Notify` + limitadores dinámicos; concurrencia de preparación/subida/descarga cambia en vivo y el worker duerme hasta el próximo retry real en vez de consultar SQLite cada pocos cientos de milisegundos.
- TDLib ahora publica `UpdateNewMessage`, `UpdateDeleteMessages` y `UpdateFile` hacia colas internas acotadas. `cloud.rs` continúa siendo dueño de interpretar/aplicar mensajes y las descargas esperan `UpdateFile` con fallback seguro.
- Añadido `nuvio-catalog-v2`: snapshot remoto comprimido y verificado por SHA-256/checkpoints para que un dispositivo nuevo pueda reconstruir el catálogo y continuar incrementalmente sin recorrer toda la historia de Telegram.
- Añadida paginación SQL del catálogo, FTS5, orden natural `NUVIO_NATURAL` y virtualización manual de grid/lista para mantener acotados IPC, memoria y DOM con bibliotecas grandes.

### Media, transferencias y Android

- Miniaturas visibles se agrupan en batches locales de hasta 32 IDs; las minithumbnails ya conocidas se resuelven desde SQLite en un único IPC y sólo los faltantes hacen fallback individual.
- La cache multimedia tiene índice persistente/LRU y evita revalidaciones físicas completas cuando el archivo cacheado ya fue verificado.
- La telemetría frecuente de transferencias vive en memoria y se persiste de forma throttled, manteniendo checkpoints inmediatos en cambios de fase/estado/error/finalización.
- Copias verificadas de escritorio calculan SHA-256 durante la misma pasada de escritura, eliminando una reread completa del temporal.
- Android conserva copy+hash en una pasada al staging de `content://`; al publicar descargas SAF se eliminó el hash redundante del source y se conserva la verificación fuerte sobre los bytes realmente persistidos en el destino.
- El generador Android moderniza el `BuildTask` de Tauri usando `ExecOperations` y `ProjectLayout`, evitando APIs Gradle retiradas/obsoletas.

### QA y rendimiento

- Graphify final reconstruido sobre la arquitectura resultante: 1,292 nodos / 3,761 relaciones.
- Suite Rust completa: 100/100 tests aprobados; benchmark de rendimiento queda ignorado por defecto y se ejecuta explícitamente con `qa:perf`.
- Benchmark release escalado a 10k/30k/100k/250k archivos. En 250k: primera página ~416 ms, FTS ~1.274 s, stats ~329 ms, carpetas ~462 ms y delta de una fila ~123 ms en el host QA.
- `cargo clippy --all-targets -- -D warnings`, `pnpm check`, build Vite de producción, `git diff --check`, QA UI completa y QA de autenticación: aprobados.
- Android ARM64: compilación Kotlin y release Rust aprobadas; pipeline valida firma APK v2/v3, RSA-3072, ARM64, `libtdjson.so`, páginas ELF de 16 KB, `targetSdkVersion 36` y `POST_NOTIFICATIONS`.
- Windows x64: build release + NSIS offline con 15 DLL x64 de TDLib/OpenSSL/zlib/VC++ aprobados. Authenticode sigue `NotSigned`, igual que releases previas; no se declara firma de editor.
- Versión promovida de 1.2.4 a 1.2.5 en package/Cargo/Tauri para no publicar una build funcionalmente distinta reutilizando el número/hash histórico de 1.2.4.

### Artefactos 1.2.5

- Windows x64 final: `release/Windows/Nuvio-Setup-Windows11-x64.exe`, 231,842,609 bytes, SHA-256 `64A14BD2E7DED2455B3747BAA4A35F23DACE5A806670E24FF45B424103CC6C54`.
- Windows NSIS interno: `Nuvio_1.2.5_x64-setup.exe`; incluye WebView2 offline y las 15 DLL x64 verificadas de TDLib/OpenSSL/zlib/VC++.
- Windows Authenticode: `NotSigned`, igual que 1.2.4; no se declara firma de editor.
- Android ARM64 final: `release/Android/Nuvio-Android-aarch64.apk`, 59,162,954 bytes, SHA-256 `463E440BDA148B71D727FDF51A806546A5A73D2194D552D66CDD4C61417D5285`.
- Android manifiesto final: `versionName=1.2.5`, `versionCode=1002005`, `minSdk=26`, `targetSdk=36`.
- APK verificado con Signature Scheme v2/v3; certificado `CN=Nuvio, O=Nuvio, C=MX`, RSA-3072, fingerprint SHA-256 `330be739ff835d1c2c1b58d057312eadb8b475286954c63cfc2bd819476872ba`.
- `libnuviodrive_v1_lib.so` y `libtdjson.so` verificados como AArch64 y con segmentos LOAD alineados a `0x4000` (16 KB).

## 1.2.4 — 2026-09-17 — Respaldos por volúmenes ZIP y release final

### Autenticación Telegram 2026 — métodos ampliados y future-auth

- Graphify se reconstruyó después de la implementación y quedó en 1,153 nodos / 3,219 aristas / 44 comunidades. Se trazó de extremo a extremo la máquina de autorización TDLib → `TelegramService` → comandos Tauri → bridge TypeScript → `TelegramConnectModal`.
- `TelegramAuthSnapshot` conserva ahora metadata que antes se descartaba: longitud del código, URL de Fragment, patrón/longitud del correo, disponibilidad de Google ID/Apple ID, estado de reset del correo y número de tokens future-auth disponibles.
- Fragment queda soportado de extremo a extremo cuando Telegram lo ofrece: TDLib entrega `authenticationCodeTypeFragment.url`, Nuvio la conserva, muestra el canal y abre exclusivamente URLs HTTPS de `fragment.com` mediante el plugin opener; el código sigue validándose por el flujo normal de TDLib.
- Google ID y Apple ID quedan conectados al backend real de TDLib mediante `EmailAddressAuthenticationGoogleId` / `EmailAddressAuthenticationAppleId`. La UI solo los muestra cuando `allow_google_id` / `allow_apple_id` llegan activos desde Telegram; los tokens se tratan como secretos temporales y se limpian del estado local después del envío.
- Añadido reset de correo de autenticación usando `resetAuthenticationEmailAddress`, incluyendo estados `available` / `pending`, cuenta regresiva y retorno seguro al flujo por teléfono cuando Telegram lo permite.
- Añadida configuración de correo de login para futuras autenticaciones: `setLoginEmailAddress`, reenvío y `checkLoginEmailAddressCode`, disponibles desde una sesión ya conectada.
- El panel de correo de login consulta `getPasswordState` + `isLoginEmailAddressRequired` antes de habilitar cambios: distingue cuentas que lo requieren, cuentas con correo ya configurado y cuentas donde Telegram no ofrece esta capacidad, evitando botones que fallen por diseño.
- Implementado future-auth real de TDLib. Nuvio captura `updateOption("authentication_token")`, conserva hasta 20 tokens deduplicados, los protege con el almacén de secretos existente (DPAPI en Windows / protección móvil en Android) y los reenvía automáticamente en `PhoneNumberAuthenticationSettings.authentication_tokens` para accesos posteriores.
- Si el usuario desactiva “Recordar sesión” o usa “Olvidar sesión”, los future-auth tokens se eliminan junto con las credenciales persistentes. La base TDLib y el ciclo de logout mantienen la semántica existente.
- Passkey / Windows Hello se muestra como no disponible para clientes no oficiales en vez de ofrecer un botón falso; Nuvio no intenta registrar una credencial que Telegram no permite usar desde un RP ID de terceros.
- Se añadió soporte opcional para credenciales de aplicación preconfiguradas mediante `NUVIO_TELEGRAM_API_ID` y `NUVIO_TELEGRAM_API_HASH`, manteniendo el formulario manual cuando no existen.
- QA de autenticación ampliado a 29 escenarios Playwright, todos aprobados: canales Telegram/SMS/llamada/correo/Fragment, reenvíos, errores y reintentos, 2FA, Google ID, Apple ID, reset de correo, future-auth, política de passkey, correo de login y layouts desktop/móvil.
- Suite Rust completa: 89/89 aprobadas. Suite UI general de producción: 31/31 aprobadas. TypeScript, build Vite, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` y `git diff --check`: aprobados.
- La primera QA nueva detectó que `codeLength` de Fragment no llegaba al `maxLength` del input. Se corrigió la propagación y la batería completa volvió a pasar.

### Archivos individuales mayores de 2 GB

- Extendido el pipeline existente de compresión previa para que un único archivo que supere el presupuesto de un ZIP pueda respaldarse automáticamente en múltiples volúmenes independientes de menos de 2 GB, incluidos archivos ya comprimidos como ZIP, RAR y 7z.
- Los volúmenes usan nombres descriptivos y ordenables, por ejemplo `Nuvio-respaldo.rar-parte-001-de-080.zip`, en lugar de nombres genéricos que dificulten identificar a qué respaldo pertenecen.
- Cada volumen contiene `NUVIO-SPLIT-MANIFEST.json` con formato `nuvio-split-v1`, nombre y tamaño del original, índice y número total de partes, tamaño y SHA-256 de la parte y SHA-256 del archivo completo.
- La división es streaming con búfer de 1 MiB; no carga el archivo completo en memoria y está diseñada para fuentes muy grandes, por ejemplo respaldos de 150 GB.
- Formatos ya comprimidos se almacenan sin recompresión innecesaria. Los archivos normales conservan la compresión rápida existente.
- Después de escribir las partes, Nuvio vuelve a abrir cada ZIP, valida su manifiesto, vuelve a leer el fragmento y recalcula SHA-256 antes de marcar los volúmenes como listos. Si el origen cambia durante el proceso, la operación se rechaza.
- El agrupado existente de selecciones con muchos archivos se conserva; si el conjunto supera el presupuesto, se generan varios ZIP independientes automáticamente.
- La UI ahora explica explícitamente que la opción puede comprimir una selección o dividir automáticamente un archivo individual grande en volúmenes verificables.

### Análisis arquitectónico

- Graphify reconstruyó el grafo local y se revisaron las rutas `archive`, `transfer`, `crypto`, `repository`, `domain`, `progress`, `telegram`, `cloud`, `media`, `mobile`, comandos Tauri, bridge, tipos y UI. La arquitectura de transferencia/catalogación puede tratar cada volumen como una subida normal independiente sin cambios de esquema en SQLite ni en Telegram.
- El defecto previo quedó localizado en `archive.rs`: un archivo individual mayor al presupuesto terminaba dentro de un grupo propio y `LimitedWriter` lo rechazaba al alcanzar el límite. El nuevo plan distingue grupos normales de fuentes que requieren división.

### Refactor final de arquitectura frontend

- `App.tsx` dejó de concentrar procesos de negocio y quedó como composición principal de UI. El archivo terminó en 1,281 líneas después de mover sincronización, lifecycle, subidas, transferencias, carpetas, papelera, mantenimiento, previews y drag & drop a hooks especializados.
- Añadidos `useCatalogSync`, `useDashboardLifecycle`, `useUploadActions`, `useTransfers`, `useFolderActions`, `useTrashActions`, `useMaintenanceActions`, `useMediaPreview`, `useFileDragDrop` y `useNativeFileDrop`.
- El bridge frontend se separó por dominios bajo `src/bridge/`, manteniendo `bridge.ts` como fachada compatible para evitar cambios masivos en consumidores existentes.
- El drag interno de archivos/carpetas y el drag nativo Explorer → Nuvio quedaron separados, conservando mouse, touch, WebView2, autoscroll y validación de ciclos de carpetas.
- La sincronización manual conserva publicación incremental mediante deltas de SQLite y el polling normal se pausa mientras hay drag activo o una sincronización manual en curso.

### QA y empaquetado final de 1.2.4

- Versión promovida de 1.2.3 a 1.2.4 en `package.json`, `src-tauri/Cargo.toml` y `src-tauri/tauri.conf.json`; Cargo confirmó `nuviodrive-v1 v1.2.4` durante QA.
- Pruebas específicas de archivo/volúmenes: 5/5 aprobadas, incluida reconstrucción byte por byte del original desde todas las partes y validación de manifiestos/SHA-256.
- Suite Rust completa: 86/86 aprobadas.
- `cargo clippy --all-targets -- -D warnings`: aprobado.
- QA de autenticación Telegram: 20/20 aprobadas, cubriendo Telegram, SMS, llamada, correo, reenvíos, errores/reintentos, 2FA y layouts desktop/móvil.
- TypeScript `tsc --noEmit` y build de producción Vite: aprobados.
- `cargo fmt --check` y `git diff --check`: aprobados después de aplicar el formato final.
- Suite UI completa: 31/31 aprobadas, incluyendo drag mouse/touch, carpetas, WebView2, previews, sincronización incremental, limpieza segura, ZIP, Upload Advisor y layouts 390/768/1280.
- Setup Windows x64 generado con Tauri/NSIS 1.2.4 e instalado silenciosamente en un entorno QA. `verify-windows.mjs` confirmó 15 DLL x64, ejecutable coincidente, marcador `UNK -> NSS` y el frontend `index-DyiU8NGq.js` embebido.
- Instalador final: `release/Windows/Nuvio-Setup-Windows11-x64.exe`, 231,633,297 bytes, SHA-256 `FFEE9D039C5B59A1E395A46A784AB7C5D541FE6C4E44A2BDEC1A57974EDB29A9`.
- Authenticode: `NotSigned`, sin cambio respecto a las builds anteriores; no se declara firma de editor.

## 1.2.3 — 2026-09-15 — Final QA / empaquetado

- Versión promovida de 1.2.2 a 1.2.3 en `package.json`, `src-tauri/Cargo.toml` y `src-tauri/tauri.conf.json` para que Windows y Android identifiquen esta build como una versión nueva y no sobrescriban silenciosamente el número anterior. Cargo regeneró `src-tauri/Cargo.lock` y confirmó `nuviodrive-v1 v1.2.3` durante QA.

### Sincronización de bibliotecas grandes

- Corregido el techo aparente de 10,000 archivos. El backfill rápido vuelve a utilizar `searchChatMessages("#Nuvio1")`, pero ahora sigue el cursor oficial de TDLib `next_from_message_id` en lugar de fabricar la paginación con el último mensaje recibido.
- Añadida detección automática de truncamiento o cursor atascado. Si la búsqueda rápida parece incompleta, Nuvio continúa con `getChatHistory` desde el punto alcanzado para no perder archivos.
- Las sincronizaciones posteriores al backfill siguen siendo incrementales mediante checkpoints, evitando recorrer nuevamente toda la biblioteca al abrir la aplicación.
- Carpetas, movimientos, papelera y metadatos de borrado también usan el cursor oficial de TDLib.
- SQLite optimizado para catálogos grandes: WAL, `synchronous=NORMAL`, temporales en memoria, caché aproximada de 64 MB, mmap de 256 MB y checkpoints WAL más espaciados.
- Benchmark local de 30,123 archivos: el trabajo SQLite bajó aproximadamente de 4.95 s a 1.49 s en la prueba de catálogo.
- La UI de sincronización usa deltas del catálogo durante el proceso para incorporar sólo filas nuevas, evitando reserializar el dashboard completo continuamente.
- Carpetas se publican antes que los archivos durante la sincronización progresiva.

### Sincronización continua — cambio actual

- La ruta rápida y el fallback de historial publican cada página recibida de Telegram inmediatamente, con un máximo de 100 mensajes por página, antes de solicitar la siguiente. Nuvio ya no acumula 500/1000 documentos antes de hacerlos visibles.
- Cada página recibida se confirma en SQLite en una transacción pequeña bajo WAL + `synchronous=NORMAL`; así la UI puede mostrar archivos de forma continua sin sacrificar la integridad del catálogo.
- La UI consulta deltas del catálogo aproximadamente cada 250 ms durante una sincronización manual, por lo que los archivos recién confirmados aparecen sin reconstruir el dashboard completo.
- Si Telegram/TDLib tarda en entregar la siguiente página, no existe información nueva que Nuvio pueda mostrar durante ese intervalo. La implementación actual minimiza la pausa controlable por Nuvio: cualquier página ya recibida se publica antes de esperar otra respuesta de red.

### Carpetas por arrastre

- Completado: mover una carpeta dentro de otra arrastrándola, reutilizando el motor Pointer Events que ya funciona para archivos en Windows/WebView2 y Android táctil.
- Validación preventiva en UI para impedir mover una carpeta dentro de sí misma o de un descendiente; el backend mantiene una segunda validación recursiva en SQLite como fuente de seguridad.
- Añadido estado visual `is-dragging` y badge de destino para carpetas.
- Añadidas pruebas UI específicas para arrastre de carpetas con mouse y touch.
- La primera ejecución de la suite completa detectó una regresión de clic simple al abrir carpetas causada por la interacción entre pointer drag y click. La corrección ya está aplicada: el clic simple vuelve a abrir la carpeta sin activar el movimiento y la prueba específica `folders-create-move-and-open` vuelve a pasar.

### Funcionalidades preservadas en este ciclo

- La aplicación no inicia una resincronización completa al abrir si el catálogo ya fue inicializado.
- Android conserva la notificación de sincronización real en segundo plano.
- Papelera/borrado sincronizados entre Windows y Android, incluyendo restore y backfill de estados legacy.
- Miniaturas borrosas/minithumbnails de Telegram, precarga de imágenes y vistas previas.
- Acción manual para liberar originales de imágenes ya subidas, independientemente del check “Liberar espacio tras subir”, verificando tamaño y SHA-256 antes de borrar.
- Arrastre interno de archivos, drag & drop externo desde Windows, carpetas jerárquicas, historial y movimientos sincronizados.

### QA acumulado de 1.2.3

- Rust: 78/78 tests aprobados en el árbol efectivo actual; la publicación continua se cubre mediante las pruebas existentes de catálogo progresivo y reconciliación.
- `cargo clippy --all-targets -- -D warnings`: aprobado.
- TypeScript `tsc --noEmit`: aprobado.
- Build de producción Vite: aprobado; permanece únicamente el warning conocido de chunk >500 kB.
- `git diff --check`: aprobado; únicamente advertencias LF→CRLF existentes.
- Prueba UI `desktop-folder-drag-into-folder`: aprobada.
- Prueba UI `touch-folder-drag-into-folder`: aprobada.
- Prueba UI `folders-create-move-and-open`: aprobada otra vez después de corregir la regresión de clic simple.
- Suite UI completa sobre `dist` 1.2.3 recién recompilado: 29/29 pruebas aprobadas, incluyendo clic simple de carpeta, arrastre de carpetas con mouse/touch, arrastre de archivos, sincronización progresiva, miniaturas, limpieza manual y layouts 390/768/1280.

### Artefactos 1.2.3

- Android ARM64 final generado y verificado nuevamente con `package-android.ps1 -VerifyOnly` el 15 de septiembre de 2026.
- APK: `release/Android/Nuvio-Android-aarch64.apk`.
- Android tamaño: 58,040,871 bytes (55.35 MB).
- Android SHA-256: `8B50F5CDF095EADE99A4BD166804DF6EFF41A9666391B149D64A6F91FA5BF68D`.
- Windows x64 final generado correctamente con Tauri/NSIS 1.2.3; el build Release terminó con código 0 y el instalador fue copiado a `release/Windows/Nuvio-Setup-Windows11-x64.exe`.
- Windows tamaño: 231,203,742 bytes (220.49 MB).
- Windows SHA-256: `6C8F5F21493BD9C50A46ED5671827B2CDA3B19A57D6DD707D69383AC62762077`.
- Windows Authenticode: `NotSigned` (sin cambio respecto a las builds anteriores; no se declara firma de editor).
- Ambos artefactos fueron generados después de los últimos cambios funcionales de 1.2.3; no queda empaquetado pendiente en este ciclo.

## 1.2.2 — 2026-09-14

- Sincronización progresiva de escritorio: los archivos se incorporan a la lista mientras Telegram sigue siendo recorrido, en lugar de aparecer sólo al final.
- La UI refresca durante una sincronización manual larga y mantiene reconciliación final de carpetas, movimientos, papelera y eliminaciones.
- Corrección del drag interno en Windows/WebView2 usando Pointer Events en lugar de HTML5 drag, conservando simultáneamente Explorer → Nuvio.
- Sincronización bidireccional de papelera/restauración entre Windows y Android mediante metadatos `#NuvioTrash1`.
- Verificación de borrado permanente en Telegram y backfill único para estados legacy de papelera.

## 1.2.1 — 2026-09-13

- Mejoras de carpetas virtuales y subcarpetas, incluyendo validación de ciclos y sincronización de la jerarquía.
- Mejoras de miniaturas y precarga multimedia para acercar la experiencia a un explorador/cloud moderno.
- Mejoras de sincronización entre clientes Windows y Android, historial, movimientos y consistencia del catálogo.

## 1.2.0 — 2026-09-11

- Opción de compresión previa en ZIP con límite estricto de 2 GB por paquete (integrada en subida de archivos, carpetas y arrastre externo desde PC).
- Creación automática de múltiples paquetes independientes si la selección supera el presupuesto de 2 GB.
- Compresión por bloques con omisión de recompresión para formatos multimedia ya comprimidos (imágenes, vídeos, zip, etc.) para acelerar el procesamiento y ahorrar memoria en Android.
- Optimización de subida y preparación: cálculo de hash SHA-256 en una sola pasada en streaming durante la copia a caché para archivos no cifrados.
- Optimización del catálogo: inserción en SQLite por lotes de 250 documentos y eliminación del reescaneo completo redundante de Telegram al vaciarse la cola de transferencias.
- Optimización de interfaz: sondeo adaptativo (750ms en transferencias activas, 2.5s en reposo, 10s en segundo plano), cálculo de dashboard en segundo plano (`spawn_blocking`) y caché de uso de disco.


## 1.1.0 — 2026-09-10

- Selección al tocar o hacer clic en cualquier zona libre de la tarjeta o fila. Los botones y casillas mantienen sus acciones independientes.
- Arrastre desde el nombre, la miniatura o los metadatos; permite mover varios archivos seleccionados a una carpeta o a Mi unidad.
- Corrección de la cancelación del arrastre de ratón causada por `pointercancel`. El arrastre HTML se activa según el puntero utilizado, sin interferir con el gesto táctil.
- Arrastre táctil desde toda la tarjeta seleccionada, desplazamiento automático cerca de los bordes y cancelación al soltar fuera de un destino. Se libera la captura del puntero al terminar.
- Miniaturas precargadas cerca del área visible, con dos preparaciones simultáneas, caché de 48 resultados y limpieza al cerrar sesión o vaciar la caché.
- Miniaturas de Telegram y vistas de imágenes, primera página de PDF y texto. Las originales precargadas se limitan a 8 MB; los textos, a 256 KB. Los videos grandes no se descargan para generar una miniatura.
- Corrección del visor de texto: rechaza archivos de más de 2 MB antes de descargarlos.
- Paquetes Windows x64 y Android ARM64 con la misma versión y código de interfaz compartido.

## Base inicial — 2026-09-09

Se incorporó el estado existente de ambas aplicaciones al control de versiones, incluyendo las correcciones de catálogo, carpetas, descargas verificadas, integración Android Keystore/SAF y empaquetado autónomo de Windows.
