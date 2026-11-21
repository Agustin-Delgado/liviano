# Funciones

## Cómo se ve

La interfaz se dibuja con Direct2D y DirectWrite sobre ventanas Win32, sin
interfaz web y sin Mica (que tiñe con el fondo de pantalla). Colores, tipo,
esquinas, sombras y movimientos siguen un mismo sistema de diseño: grises
neutros, un índigo para lo activo y nada más pesado que medium. La tabla con
cada token está en [diseno.md](diseno.md).

Todo se mueve con curvas y duraciones medidas:

- el hover de pestañas y botones (150 ms);
- la tarjeta de la pestaña activa, que se desliza hasta la elegida;
- las vecinas, que hacen lugar al abrir, cerrar o reordenar;
- la columna, que vuelve desde el borde;
- la línea de foco de la vista dividida;
- el campo único, la tarjeta, Ajustes y la barra de contraseñas, que
  aparecen y se van con un fundido real;
- los toasts, con su resorte;
- las casillas de Ajustes.

Una animación interrumpida sigue desde donde está. Nada bloquea la
entrada. En reposo no hay timer: 0 % de CPU. Si Windows tiene las
animaciones apagadas, todo llega de una.

No hay barra de título: las pestañas ocupan su lugar y los botones de la
ventana son los de Windows en chico, al principio de la fila: minimizar,
maximizar y cerrar, con glifos finos de 8 px sobre una cápsula tenue de tres
segmentos de 28 px (los botones de Windows 11 miden 46), separada de las
flechas. Con las pestañas arriba van antes de las pestañas; al costado, arriba
a la izquierda de la columna; ocultas, a la izquierda de la fila de la
ventana. Al pasar el puntero se oscurece su segmento, rojo bajo cerrar, y con
la ventana inactiva sus glifos se apagan. Sobre maximizar
aparecen los diseños de ajuste de Windows.

Al lado de esos botones están **Atrás, Adelante y Recargar** (al costado, a la
derecha de esa misma fila, como en Arc). Siguen a la pestaña activa: una
flecha sin adónde ir se ve apagada y no responde. Clic derecho, o mantener
apretada una flecha, abre el historial de la pestaña hacia ese lado (hasta
quince entradas, la más cercana primero). Los botones 4 y 5 del mouse van
atrás y adelante sobre la fila; sobre la página los maneja la propia página.

El engranaje abre Ajustes y está siempre a la vista:

- con las pestañas arriba, al final de la fila, a la derecha de las
  extensiones;
- con las pestañas al costado, al pie de la columna;
- con las pestañas ocultas, al final de la fila de la ventana.

Al dejar el puntero encima muestra el tooltip «Ajustes (Ctrl+,)».

Cada pestaña lleva el ícono de su sitio (sin ícono, o privada, su inicial).
Con el puntero quieto sobre una pestaña aparece, como en Chrome, una tarjeta
con el título entero, el sitio, si está dormida, suspendida o siempre
despierta, y una vista chica de cómo estaba la página la última vez que se la
vio (sólo en memoria, 3 MB entre todas); una pestaña dormida conserva la suya.
Pasar a otra pestaña la mueve sin esperar, y un clic, arrastrar o abrir el
campo la cierra. La vista previa se apaga en Ajustes → Pestañas (la tarjeta
queda).

| | |
|---|---|
| ![Pestañas arriba, tema claro](capturas/tira-arriba-claro.png) | ![Pestañas arriba, tema oscuro](capturas/tira-arriba-oscuro.png) |
| ![Pestañas al costado, tema claro](capturas/tira-costado-claro.png) | ![Pestañas al costado, tema oscuro](capturas/tira-costado-oscuro.png) |
| ![Botones de la ventana con el puntero encima](capturas/tira-arriba-claro-botones.png) | ![Botones de la ventana al costado, tema oscuro](capturas/tira-costado-oscuro-botones.png) |
| ![El engranaje con su tooltip](capturas/engranaje-claro.png) | ![El engranaje con su tooltip, tema oscuro](capturas/engranaje-oscuro.png) |
| ![Campo único abierto](capturas/campo-claro.png) | ![Campo único, tema oscuro](capturas/campo-oscuro.png) |
| ![Ajustes, tema claro](capturas/ajustes-claro.png) | ![Ajustes, tema oscuro](capturas/ajustes-oscuro.png) |
| ![Pestañas ocultas: sólo la fila de la ventana](capturas/claro-colapsado.png) | ![Historial, tema oscuro](capturas/historial-oscuro.png) |
| ![Tarjeta de una pestaña, tema claro](capturas/tarjeta-claro.png) | ![Tarjeta de una pestaña al costado, tema oscuro](capturas/tarjeta-oscuro-vertical.png) |
| ![Grupos contraídos arriba, tema claro](capturas/tira-arriba-claro-contraidos.png) | ![Grupos contraídos arriba, tema oscuro](capturas/tira-arriba-oscuro-contraidos.png) |
| ![Grupos al costado contraídos, tema claro](capturas/tira-costado-claro-contraidos.png) | ![Grupos al costado contraídos, tema oscuro](capturas/tira-costado-oscuro-contraidos.png) |
| ![Menú de la etiqueta de un grupo](capturas/grupos-claro-menu-grupo.png) | ![Menú de una pestaña: agregarla a un grupo](capturas/grupos-claro-menu-pestana.png) |

Cuadros intermedios, con el tiempo de cada uno, grabados con
`tools/frames.ps1`:

| | |
|---|---|
| ![Hover de una pestaña, 150 ms](capturas/tira-hover-claro.png) | ![Cambio de pestaña: la tarjeta se desliza](capturas/tira-pestana-oscuro.png) |
| ![La columna vuelve desde el borde](capturas/tira-columna-claro.png) | ![Tooltip del engranaje: 600 ms de demora y fundido](capturas/tira-engranaje-oscuro.png) |
| ![Un grupo que se contrae arriba, 220 ms](capturas/tira-contraer-arriba.png) | ![Un grupo que se contrae en la columna](capturas/tira-contraer-costado.png) |

Las capturas salen de `tools/capture.ps1` (PrintWindow, sin traer la ventana
al frente); la sombra y el borde que dibuja Windows no aparecen en ellas.

Recorrido completo, con una foto por función y un video:
[docs/muestra](muestra/README.md).

![Tarjeta al pasar el mouse, cambio de pestaña y reordenar](muestra/animaciones.gif)

## Estado de cada función

Estado de cada función en esta versión.
"Verificado" quiere decir probado con el programa compilado, manejado por el
canal de control por script; lo que no se puede probar así está marcado.

| Función | Estado |
|---|---|
| Campo único para direcciones y búsquedas | Verificado. Nada sale hasta Enter. |
| Autocompletado desde el historial local | Verificado (sugerencias y completado en línea). |
| Pestañas arriba o al costado, barra lateral plegable | Verificado. Se elige en Ajustes → Pestañas → Ubicación, en el menú de clic derecho de la fila (o de la ventana) y con Ctrl+Shift+S. |
| Pestañas fijadas | Verificado; persisten entre sesiones. |
| Restauración diferida | Verificado: al abrir sólo se crea la vista de la pestaña activa (y la de su par, si está dividida). |
| Cambiar de pestaña por nombre | Verificado (búsqueda difusa por título y dirección). |
| Dormir y descartar pestañas de fondo | Verificado: se suspenden pasado el tiempo de Ajustes y se descartan después, o enseguida con poca memoria. |
| Mantener un sitio despierto | Verificado. |
| Vista dividida | Verificado (pares, persistencia, privadas). El arrastre de pestañas y del divisor se revisa a mano. |
| Grupos de pestañas | Verificado: crear, nombrar, color, contraer y expandir con un clic, persistencia (las privadas no se guardan) y pares divididos dentro de un grupo. El arrastre se probó con mensajes de mouse a la ventana propia; el de la etiqueta se revisa a mano. |
| Modo lectura | Verificado (Readability). |
| Ocultar elementos por sitio | Verificado (selección por clic, se aplica antes de pintar). La lista de ocultos (Ctrl+Shift+U) es un menú: se revisa a mano. |
| Video flotante | Hecho con el Picture-in-Picture del motor. Sin verificar en esta pasada (hace falta una página con video). |
| Bloqueo de anuncios y trackers | Verificado, con excepción por sitio (menú de la pestaña). Usa EasyList y EasyPrivacy, con una lista mínima incluida mientras se descargan. Ver [privacidad.md](privacidad.md). |
| Ventanas emergentes | Verificado con páginas locales: se bloquean las que nadie pidió y las de anuncios; se permiten por sitio desde el menú de la pestaña. |
| Rastreadores y carteles de cookies | Verificado: prevención de rastreo estricta del motor y carteles ocultos antes del primer cuadro (EasyList Cookie List). |
| Contraseñas en el Administrador de credenciales | Verificado: ofrece guardar tras un login y nunca rellena solo. Ctrl+Alt+L abre el panel. |
| Importar contraseñas de otros navegadores | Hecho (Chrome, Edge, Brave, Vivaldi, Opera, Arc y CSV, todo en el equipo). Sin verificar en esta pasada: las pruebas no leen perfiles reales. |
| Llaves de acceso | El motor deriva WebAuthn al diálogo de Windows Hello. Sin verificar en esta pasada. |
| Tema claro, oscuro o del sistema | Verificado claro y oscuro; "del sistema" sigue el cambio de Windows. |
| Marcadores, historial y descargas | Verificado. |
| Pestañas privadas | Verificado: perfil InPrivate propio; nada va al historial, a la sesión, a Ctrl+Shift+T ni a settings.json. |
| Extensiones de Chrome | Verificado: carga, botón (sólo si hay extensiones) y fijadas. Limitaciones en [docs/extensiones.md](extensiones.md). |
| Buscar en la página | Verificado. |
| Actualización diaria firmada | Hecho, con pruebas unitarias. Apunta a los releases de este repositorio; mientras no haya ninguno, no encuentra nada. |
| Control por script | Verificado (ver [compilar.md](compilar.md#control-por-script)). |
| Ajustes | Verificado: general, pestañas, contraseñas, extensiones, atajos, actualizaciones, privacidad y pruebas; todo se guarda al momento. |

La memoria medida contra Edge está en [docs/medicion.md](medicion.md).

## Grupos de pestañas

Como en Chrome y Edge. Clic derecho en una pestaña: "Agregar pestaña a un
grupo nuevo" (Ctrl+Shift+G), "Agregar al grupo" con los grupos que haya, o
"Quitar del grupo". El grupo nuevo pide su nombre en el campo único; Enter
vacío lo deja sin nombre.

- **Arriba**, cada grupo es una etiqueta antes de sus pestañas: el nombre
  escrito en el color del grupo sobre un tinte muy tenue de ese color (sin
  nombre, sólo un punto), y una línea fina del mismo color las une.
  **Al costado**, es una fila con un punto, el nombre en su color (sin nombre,
  el sitio de la primera pestaña) y una flecha; sus pestañas van debajo, un
  poco corridas, con una guía fina a la izquierda.
- **Un clic en la etiqueta** contrae el grupo en 220 ms: las pestañas se
  pliegan y se desvanecen, y la flecha gira. Sólo queda la etiqueta (al
  costado, con cuántas pestañas tiene). Si la pestaña activa estaba ahí,
  pasa a una visible. Ctrl+Tab se saltea las contraídas; elegir una por
  Ctrl+K o Ctrl+1…8 expande su grupo.
- **Clic derecho en la etiqueta**: renombrar, color (nueve, los tonos de la
  marca, legibles en claro y oscuro), nueva pestaña en el grupo,
  contraer o expandir, desagrupar y cerrar el grupo.
- **Arrastrar**: una pestaña soltada sobre otra entra en su grupo; sobre una
  etiqueta queda justo antes de ese grupo, afuera; más allá de la última,
  al final y sin grupo. Arrastrar la etiqueta mueve el grupo entero. Las
  pestañas de un grupo siempre quedan juntas.
- Las fijadas no van en grupos. Las privadas sí, pero no se guardan: el
  grupo vuelve en la próxima sesión sin ellas. Un par de la vista dividida
  queda siempre en el mismo grupo.

Los comandos "Quitar pestaña del grupo" y "Contraer o expandir el grupo" no
tienen atajo de fábrica; se les puede asignar uno en Ajustes.

## Lo que se revisa a mano

Se revisan a mano: arrastrar pestañas y el divisor de la vista dividida, los
menús (pestaña, extensiones, elementos ocultos, cuentas de un login), el video
flotante, la importación de contraseñas y las llaves de acceso.
