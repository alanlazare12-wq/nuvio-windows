# Historial de Nuvio

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
