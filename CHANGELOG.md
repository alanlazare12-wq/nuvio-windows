# Historial de Nuvio

> Archivo de continuidad del proyecto. A partir del ciclo 1.2.3-dev se actualiza durante el desarrollo cada vez que se incorpora o corrige una funcionalidad relevante, antes de dar el trabajo por terminado. Cada entrada conserva decisiones técnicas, QA y pendientes para poder retomar el proyecto sin depender del historial del chat.

## 1.2.4 — 2026-09-17 — Respaldos por volúmenes ZIP y release final

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
- Setup Windows x64 generado con Tauri/NSIS 1.2.4 e instalado silenciosamente en un entorno QA. `verify-windows.mjs` confirmó 15 DLL x64, ejecutable coincidente, marcador `UNK -> NSS` y el frontend `index-CxlZVdPj.js` embebido.
- Instalador final: `release/Windows/Nuvio-Setup-Windows11-x64.exe`, 231,639,502 bytes, SHA-256 `D201E7E8F33350BB100B822CFB0BF63D2F719C0009BC7E976934CF4E7A58199C`.
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
