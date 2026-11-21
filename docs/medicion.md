# Medición de memoria

Generado por `tools/measure.ps1`. Memoria de trabajo privada: `nea.exe` más todo el árbol de procesos del motor (WebView2), mediana de 3 muestras tras 10 s de asentamiento. Cada escenario arranca de cero con datos temporales.

- Fecha: 2026-11-20 15:49
- Commit: 1a410d7
- Windows: Microsoft Windows 11 Pro 10.0.26200, 32 GB de RAM
- Motor WebView2: 157.0.4341.60
- Edge: 157.0.4341.60 (perfil temporal propio, mismas páginas)
- Chrome: 156.0.8078.80 (perfil temporal propio, mismas páginas)
- Listas de bloqueo: easylist.txt (2023 KB), easyprivacy.txt (1485 KB) más la mínima incorporada
- Página liviana: https://example.com/ · página pesada: https://www.infobae.com/

| Escenario | Nea (MB) | programa | motor | procesos | Edge (MB) | Ahorro | Chrome (MB) | Ahorro | Nota |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| solo-programa | 1.6 | 1.6 | 0.0 | 1 |  |  |  |  |  |
| about-blank | 86.8 | 4.1 | 82.7 | 7 | 184.1 | 53 % | 239.7 | 64 % |  |
| liviana | 109.8 | 3.9 | 105.9 | 7 | 201.2 | 45 % | 246.8 | 56 % |  |
| pesada-con-bloqueo | 394.9 | 4.3 | 390.6 | 9 | 641.4 | 38 % | 783.9 | 50 % | 44 de 197 solicitudes bloqueadas |
| pesada-sin-bloqueo | 559.3 | 4.3 | 555.0 | 16 | 641.4 | 13 % | 783.9 | 29 % | 0 de 306 solicitudes bloqueadas |
| 20-pestanas | 182.7 | 5.0 | 177.7 | 7 | 451.3 | 60 % | 491.2 | 63 % | 19 dormidas |
| restaurar-50 | 109.8 | 3.9 | 105.8 | 7 |  |  |  |  | 50 pestañas restauradas |

## Presupuestos de memoria

| Presupuesto o criterio | Objetivo | Medido | ¿Se cumple? |
|---|---|---|---|
| Proceso propio (interfaz + lógica) | < 20 MB en reposo (máx. unas pocas decenas) | 1.6 MB sin vistas; máximo 5.0 MB en todos los escenarios | Sí |
| Pestaña dormida (sólo metadatos) | < 0,5 MB cada una | Menos de 0.002 MB (bajo la resolución) del programa por pestaña (restaurar-50 contra liviana, 49 dormidas); contando el motor, 0.00 MB | Sí |
| Motor de filtros | Pocos MB | 1.4 MB de reglas compiladas (106,608 reglas de red) | Sí |
| Pestaña activa con página liviana | 80 a 150 MB entre todos los procesos del motor | 105.9 MB de motor, 109.8 MB en total | Sí |
| 20 pestañas con 19 dormidas, claramente menos que Edge | Al menos 40 % menos | 182.7 MB contra 451.3 MB: 60 % menos | Sí |
| 20 pestañas con 19 dormidas, claramente menos que Chrome | Al menos 40 % menos | 182.7 MB contra 491.2 MB: 63 % menos | Sí |
| Pestaña suspendida | Reducción fuerte respecto de la activa | Sin escenario propio: "20-pestanas" duerme con descarte (vista destruida), el estado de menor costo | Sin medir |
| Índice de historial | Pocos MB con decenas de miles de entradas | Los escenarios arrancan con historial vacío | Sin medir |

Notas:

- "programa" es `nea.exe`; "motor" suma todos sus procesos hijos (navegador, GPU, red, almacenamiento, renderizadores, crashpad de WebView2).
- Las variantes de la página pesada difieren sólo en settings.json (`con-bloqueo: {}`, `sin-bloqueo: {"shield_disabled":["infobae.com"]}`); la clave de `shield_disabled` es la de `shield::site_key` (el host sin "www."). La nota cuenta las solicitudes que vio y bloqueó el motor de filtros desde que se abrió la página hasta la última muestra.
- En Edge y Chrome no se puede forzar el sueño de pestañas: en "20-pestanas" las 20 están vivas, que es lo que cuesta abrirlas ahí. Tampoco avisan cuándo terminó de cargar: se les da una espera fija antes del asentamiento. Su número incluye su propia interfaz (proceso del navegador), que en Nea es "programa".
- "restaurar-50" arranca con una sesión de 50 pestañas: sólo la activa crea su vista.
- Umbral de regresión: `tools/measure.ps1 -Baseline docs/medicion-baseline.json` sale con código 1 si un escenario supera su cifra base más la tolerancia del archivo; `-UpdateBaseline` la reescribe.
