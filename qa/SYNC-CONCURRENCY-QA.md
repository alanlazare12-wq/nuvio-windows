# Sincronización, filtros y concurrencia — 2026-09-11

Cambios aplicados sobre las modificaciones locales existentes, conservando las particularidades de Windows y Android.

- Progreso real de exploración del historial con recuento total opcional y tiempo restante aproximado; la aplicación del catálogo ocupa la fase final y solo un resultado satisfactorio llega al 100%.
- Serialización de sincronizaciones y estado de error/interrupción; sin inventar porcentaje cuando se desconoce el total.
- Orden natural español para archivos y carpetas; Recientes deja de truncarse a doce elementos y su contador coincide con el catálogo.
- Ocho subidas por defecto, selector de 1 a 16 y aplicación del límite en la cola en ejecución. Reducir el límite deja terminar los trabajos ya activos.
- Migración única de la antigua configuración predeterminada de cuatro; se respeta posteriormente la preferencia del usuario, incluida una sola subida.

Validación: ver resultados locales `ui-results.json`, pruebas Rust, registros de firma/alineación Android y verificación de instalación Windows. Las pruebas de interfaz usan IPC simulado; no constituyen una prueba de velocidad de red con Telegram. Sin S24 Ultra conectado, la verificación del APK no certifica su funcionamiento en ese dispositivo físico.

Resultado Windows: 43 pruebas Rust y 22 pruebas de interfaz aprobadas; TypeScript, compilación de producción y Clippy aprobados. Setup instalado en QA: ejecutable e interfaz actual index-CPevrgNC.js verificados y 15 DLL comprobadas. Inicio autónomo con servidores Vite detenidos: proceso vivo, ventana creada y respuesta correcta. SHA-256 del instalador: 61cb0026b94eec502d2c36ce030807d67ddddc6bd5ae0923016a0c492ab28f73.
