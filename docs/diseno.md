# Diseño y movimiento

La interfaz nativa sigue el lenguaje de un sistema de diseño web: grises
neutros, un índigo para lo activo, siete pasos de tipo y nada más pesado que
medium. En el código, los tokens están en `src/design.rs` y los movimientos en
`src/anim.rs` (`curves` y `specs`).

## Tokens

| Token | Valor en Nea |
|---|---|
| Lienzo de la página / del campo | `canvas` / `field` #FAFAFA |
| Fondo de la ventana (shell) | `chrome` #F5F5F5 |
| Hover / fila | #F0F0F0 / #F3F3F3 |
| Tinta | ink #171717, quiet #525252, muted #696969, subtle #929292 |
| Línea | #E8E8E8, fuerte #D7D7D7 |
| Marca (índigo) | #6468F0, hover #5756E4, texto encima #FCFCFC |
| Tooltip | fondo tinta #171717, texto #FAFAFA, caption 11 px medium, padding 8×2, separación 8 px |
| Tipo | caption 11, small 12, body 13, emphasis 14; peso máximo 500 |
| Esquinas | mark 2, chip 4, control 6, field 8, panel 12 |
| Sombras | `shadow-card`: anillo 1 px negro/0.08 + caída 0 2 6 −2 negro/0.12; halo de foco brand/0.18 |
| Botón | alto 26 px, esquina field, primario de marca, secundario con contorno |
| Casilla | esquina 4, borde `line-strong`, hover brand/40, marcada en brand con tilde blanco |
| Grupos de pestañas | nueve tonos: gris, azul, rojo, amarillo, verde, rosa, violeta, cian y naranja; en claro el paso `ink` del tono, en oscuro el `solid` (el relleno, ≥ 3:1 sobre la tira). El nombre se escribe en el color del grupo, llevado hacia la tinta un 20 % (25 % en oscuro), sobre un tinte del tono al 10 % (14 % en oscuro): ≥ 4.5:1 sobre la tira, sobre el tinte y con hover (lo comprueba un test). Arriba: etiqueta de alto 22, esquina control, texto small (12) medium, como el título de una pestaña; sin nombre, un punto de 6 px; línea de 1.5 px. Al costado: fila de 26, punto de 6 px, nombre (o el sitio de la primera pestaña), guía de 1 px y pestañas corridas 12 px |
| Botones de la ventana | los glifos de Windows (guion, cuadrado, cruz) de 8 px en trazo de 1 px sobre píxeles enteros, en una cápsula de tres segmentos de 28 × 28 a 6 px del borde izquierdo, radio 7, fondo `chrome` 5 % hacia `ink`; 12 px hasta Atrás; al pasar el puntero el segmento baja otro 6 % (11 % apretado), rojo #C42B1C con tinta blanca bajo cerrar, y los extremos conservan la curva de la cápsula; `ink_muted` en reposo, `ink_asleep` con la ventana inactiva |
| Atrás, adelante, recargar | botones de 28×28, fondo fantasma con hover; apagados en `subtle` sin hover; trazo de 1.5 px |

El tema oscuro es nuestro, porque el sistema de diseño no tiene. Mantiene los mismos
pasos y roles, invertidos. El índigo sube a #818CF9 para leerse sobre
fondo oscuro.

Algunas cosas de Search no chocan con ese lenguaje y quedaron igual:

- las pestañas como fondos suaves sobre la tira;
- el violeta de la pestaña privada.

## Movimiento

| Qué se mueve | Duración y curva |
|---|---|
| Hover y press de botones, pestañas y filas | 150 ms, `ease-out` (0, 0, 0.2, 1) |
| Campo único y su lista | Entra como un panel de macOS (Spotlight): crece desde 0,96 con el resorte en 320 ms (casi todo en los primeros 100 ms) y el fundido llega en 140 ms, `ease-out`. Sale en 150 ms, `ease`: se encoge a 0,96 y se desvanece. Al medio crece desde su centro; arriba y abajo, desde el borde del que cuelga, con 4 px de viaje |
| Menú | 120 ms entrada, 100 ms salida, `ease` |
| Ajustes (diálogo) | 100 ms, `ease` |
| Tooltip | demora 600 ms, entrada 100 ms, salida 80 ms; cierre tras 100 ms; si otro tooltip cerró hace menos de 300 ms, abre sin demora |
| Toast y barra de contraseñas | entrada 396 ms con resorte `linear()` (pasa a 1.038 y asienta), sube 6 px; salida 400 ms |
| Línea de foco de la vista dividida | 200 ms, `ease-out` |
| Fondo de la pestaña activa al cambiar de pestaña | 140 ms, `--ease-out-strong` (a 200 ms, cruzar toda la tira se sentía lento) |
| Vecinas que hacen lugar (reordenar, abrir, cerrar) | 160 ms y 200 ms, `--ease-out-strong` (0.23, 1, 0.32, 1) |
| Columna de pestañas que vuelve | 250 ms, `--ease-move` (0.77, 0, 0.175, 1) |
| Grupo que se contrae o se expande | 220 ms, `--ease-out-strong`: sus pestañas se pliegan hacia la etiqueta (o salen de ella) mientras se desvanecen, la flecha gira, la línea del grupo se apaga y todo lo de al lado acompaña con la misma curva. Si la pestaña activa queda adentro, su fondo no viaja: aparece con un fundido en la que la reemplaza |
| Etiqueta de un grupo nuevo | 200 ms, `--ease-out-strong`, se despliega y aparece con un fundido, como una pestaña que se abre |
| Tilde de la casilla | 75 ms |
| Anillo de carga | 1 vuelta/s, a 20 cuadros/s, como `animate-spin` de Tailwind |

Las curvas CSS se resuelven como lo hace el navegador:

- el `cubic-bezier` con Newton y, si hace falta, bisección;
- el `linear()` del resorte con sus mismos puntos.

Los tests las comparan con valores de Chromium.

Una animación interrumpida sigue desde donde está y no salta. Cuando un
hover se da vuelta a mitad de camino, la vuelta tarda en proporción a lo
recorrido.

Si Windows tiene apagadas las animaciones de la interfaz
(`SPI_GETCLIENTAREAANIMATION`), todo llega a su lugar de una.

## Desvíos a propósito

- **El campo se escala con una foto de sí mismo.** Una ventana con un
  `Edit` nativo no se escala. Mientras entra o sale, la ventana del
  `Edit` queda invisible (opacidad 0, con el teclado ya tomado) y la ventana
  de fondo dibuja el campo entero: una foto en reposo (su fondo, la sombra
  y lo que pintan el campo y sus hijos con `WM_PRINT`) remuestreada en cada
  cuadro a la escala y opacidad del momento. El último cuadro es la foto
  tal cual, así que el traspaso a las ventanas reales no cambia ningún
  píxel. La foto y el bitmap del cuadro se sueltan al terminar. Ajustes no
  se escala: sólo se funde.
- **El placeholder del campo es nuestro.** El cue banner del `Edit` usa el
  gris del sistema y no se le puede cambiar el color. Con el campo vacío lo
  dibujamos en `ink-muted` (más de 4,5:1 en claro y oscuro), con la fuente
  del `Edit` y donde iría su primer carácter.
- **El campo es una píldora en dos ventanas.** Windows sólo redondea
  unos pocos píxeles fijos. Debajo va una ventana con alfa por píxel
  (`UpdateLayeredWindow`) con la forma suavizada, el anillo y una sombra
  en tres capas: de contacto, principal y ambiente, amplia y difusa, como
  los paneles de macOS. Encima va la ventana con el `Edit` y la lista, recortada
  por una región un píxel adentro del borde. Los escalones de la región
  caen sobre el fondo liso de abajo, así que el borde que se ve es el
  suavizado. Con la lista abierta, el lado del campo queda redondo y el
  de la lista lleva la esquina de diálogo (16 px).
- **Abajo, la lista se lee hacia arriba.** La primera sugerencia queda
  pegada al campo y Arriba entra en la lista, como en fzf. Buscar en la
  página nunca va al medio: la página centra cada coincidencia justo ahí.
- **La columna sólo se anima al volver.** Al ocultarse desaparece de una:
  así la página gana el lugar sin esperar.
- **Menús y combos.** No hay menú contextual propio: los menús son los de
  Windows (`TrackPopupMenu`), igual que las listas de los combos, y
  conservan las transiciones del sistema.
- **Sin `DwmFlush`.** Bloquea el hilo de la interfaz hasta el próximo
  cuadro del compositor. El reloj es un único timer de 15 ms que sólo
  existe mientras algo se mueve.
- **El grupo se renombra en el campo único.** Chrome abre una burbuja con
  el nombre y la paleta. Acá el nombre se escribe en el campo de siempre y
  el color se elige en el menú de la etiqueta: no hay otra ventana que
  mantener.
- **Sin nombre accesible en el engranaje.** Ningún botón dibujado tiene
  UIA todavía; hacerlo es un trabajo aparte para toda la barra.

## Cómo verlo

- `tools/frames.ps1` graba los cuadros intermedios de una animación. Corre
  el disparador, captura con PrintWindow cada N ms, compone cada ventana
  en capas con su opacidad del momento y arma una tira. Las tiras están en
  `docs/capturas/tira-*.png`. Cada cuadro tarda unos 300 ms en capturarse
  (PrintWindow de la ventana entera con su WebView), así que una animación
  corta no deja cuadros intermedios. Las tiras `campo2-abrir-*` y
  `campo2-cerrar-*` se grabaron con las duraciones del campo multiplicadas
  por 8, en un build sólo para eso.
- `NEA_BENCH=1` agrega al `state` del pipe la clave `anim`, con
  cuadros, peor cuadro y cuadros de más de 16 ms.

## Logo

Una pluma blanca sobre un cuadrado de esquinas redondeadas en grafito
(de #51515C arriba a #27272F abajo). La pluma dice «liviandad» sin palabras
y, inclinada hacia arriba, también dice que avanza. El grafito es neutro
a propósito: el índigo queda para lo activo dentro de la ventana.

La pluma no está dibujada a mano. Sale de unos pocos números en
`tools/logo.py`: el raquis curvo, el ancho de cada paleta a lo largo de
él, el ángulo de las barbas y los lugares donde las barbas se separan.

- `python -I tools/logo.py` es la fuente y escribe todo lo demás:
  `assets/logo/nea.svg` (claro), `nea-dark.svg` (oscuro: pluma
  blanca y plata sobre casi negro), `nea-mono.svg` (la pluma sola en
  `currentColor`), `nea.ico` (16 a 64 en BMP, 256 en PNG) y las
  vistas previas de `docs/logo/`. Los SVG no se editan.
- Por debajo de 48 px las barbas y los cortes finos se empastan, así que
  los tamaños chicos salen de una pluma más simple (tabla `DETAIL`): más
  grande dentro del cuadrado, sin barbas, con un solo corte profundo y el
  raquis más grueso.
- `build.rs` embebe el `.ico` como recurso 1 con `nea.rc`. El exe
  lleva sólo el claro: Windows no cambia el ícono de un exe con el tema.
