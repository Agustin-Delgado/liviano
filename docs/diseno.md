# Diseño y movimiento

La interfaz nativa habla el idioma de celagem-svelte. En el código, los
tokens están en `src/design.rs` y los movimientos en `src/anim.rs`
(`curves` y `specs`). Las rutas de **celagem** son relativas a
`celagem-svelte/`. Las de **HKUI** son relativas a
`svelte-components/packages/ui/`.

## Tokens

| Token | Valor en Liviano | Fuente |
|---|---|---|
| Lienzo de la página / del campo | `canvas` / `field` #FAFAFA | celagem `src/lib/styles/brand.css` (`--color-canvas`, oklch → sRGB) |
| Fondo de la ventana (shell) | `chrome` #F5F5F5 | celagem `brand.css` (`--color-surface-muted`) |
| Hover / fila | #F0F0F0 / #F3F3F3 | celagem `brand.css` (`--color-hover`, `--color-row-hover`) |
| Tinta | ink #171717, quiet #525252, muted #696969, subtle #929292 | celagem `brand.css` (`--color-ink*`) |
| Línea | #E8E8E8, fuerte #D7D7D7 | celagem `brand.css` (`--color-line*`) |
| Marca (índigo) | #6468F0, hover #5756E4, texto encima #FCFCFC | celagem `brand.css` (`--color-brand`, `-hover`, `on-brand`) |
| Tooltip | fondo tinta #171717, texto #FAFAFA, caption 11 px medium, padding 8×2, separación 8 px | celagem `src/lib/components/tooltip/`; HKUI `src/lib/tooltip/` |
| Tipo | caption 11, small 12, body 13, emphasis 14; peso máximo 500 | celagem `brand.css` (`--text-*`) y la regla «Medium is the heaviest weight» de su `CLAUDE.md` |
| Esquinas | mark 2, chip 4, control 6, field 8, panel 12 | celagem `brand.css` (`--radius-*`) |
| Sombras | `shadow-card`: anillo 1 px negro/0.08 + caída 0 2 6 −2 negro/0.12; halo de foco brand/0.18 | celagem `brand.css` (`--shadow-card`, `--shadow-focus`) |
| Botón | alto 26 px, esquina field, primario de marca, secundario con contorno | celagem `src/lib/components/button/recipe.ts` |
| Casilla | esquina 4, borde `line-strong`, hover brand/40, marcada en brand con tilde blanco | celagem `src/lib/components/checkbox/` (receta de HKUI) |

El tema oscuro es nuestro, porque celagem no tiene. Mantiene los mismos
pasos y roles, invertidos. El índigo sube a #818CF9 para leerse sobre
fondo oscuro.

Algunas cosas de Search no chocan con celagem y quedaron igual:

- las pestañas como fondos suaves sobre la tira;
- el violeta de la pestaña privada.

## Movimiento

| Qué se mueve | Duración y curva | Fuente |
|---|---|---|
| Hover y press de botones, pestañas y filas | 150 ms, `ease-out` (0, 0, 0.2, 1) | celagem `button/recipe.ts` (`transition-colors duration-150`) |
| Campo único y su lista (popover) | 100 ms entrada y salida, `ease` | celagem `src/lib/components/popover/motion.ts` |
| Menú | 120 ms entrada, 100 ms salida, `ease` | celagem `src/lib/components/menu/recipe.ts` |
| Ajustes (diálogo) | 100 ms, `ease` | celagem `src/lib/components/dialog/recipe.ts` |
| Tooltip | demora 600 ms, entrada 100 ms, salida 80 ms; cierre tras 100 ms; si otro tooltip cerró hace menos de 300 ms, abre sin demora | HKUI `src/lib/tooltip/root/tooltip-root.svelte`, `tooltip-group.ts`, `provider/tooltip-provider.svelte` |
| Toast y barra de contraseñas | entrada 396 ms con resorte `linear()` (pasa a 1.038 y asienta), sube 6 px; salida 400 ms | celagem `src/lib/components/toast/styles.css` (`--toast-spring-easing`), `src/lib/motion/spring.ts` |
| Indicador de la pestaña activa y línea de foco de la vista dividida | 200 ms, `ease-out` | celagem `src/lib/components/tabs/indicator.svelte` |
| Vecinas que hacen lugar (reordenar, abrir, cerrar) | 160 ms y 200 ms, `--ease-out-strong` (0.23, 1, 0.32, 1) | celagem `brand.css` |
| Columna de pestañas que vuelve | 250 ms, `--ease-move` (0.77, 0, 0.175, 1) | celagem `brand.css`; `app-sidebar/rail-module.svelte` |
| Tilde de la casilla | 75 ms | HKUI, receta del checkbox |
| Anillo de carga | 1 vuelta/s, a 20 cuadros/s | `animate-spin` de Tailwind |

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

- **Sin escala en el popover ni en el diálogo.** Son ventanas con
  controles nativos adentro (el campo es un `Edit`, Ajustes son controles
  de Windows), y una ventana Win32 no se escala sin pixelarse. Lo que sí
  hacen es un fundido real de la ventana entera, con sus hijos, en
  ventanas en capas.
- **La columna sólo se anima al volver.** Al ocultarse desaparece de una:
  así la página gana el lugar sin esperar.
- **Menús y combos.** No hay menú contextual propio: los menús son los de
  Windows (`TrackPopupMenu`), igual que las listas de los combos, y
  conservan las transiciones del sistema.
- **Sin `DwmFlush`.** Bloquea el hilo de la interfaz hasta el próximo
  cuadro del compositor. El reloj es un único timer de 15 ms que sólo
  existe mientras algo se mueve.
- **Sin nombre accesible en el engranaje.** Ningún botón dibujado tiene
  UIA todavía; hacerlo es un trabajo aparte para toda la barra.

## Cómo verlo

- `tools/frames.ps1` graba los cuadros intermedios de una animación. Corre
  el disparador, captura con PrintWindow cada N ms, compone cada ventana
  en capas con su opacidad del momento y arma una tira. Las tiras están en
  `docs/capturas/tira-*.png`.
- `LIVIANO_BENCH=1` agrega al `state` del pipe la clave `anim`, con
  cuadros, peor cuadro y cuadros de más de 16 ms.
