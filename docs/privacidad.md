# Privacidad y datos

## Bloqueo y privacidad

Todo sale del mismo lugar: el menú de la pestaña para un sitio, y Ajustes →
Privacidad para todos.

- **Anuncios y rastreadores (red).** EasyList y EasyPrivacy, bajadas una vez
  por día. "Bloqueo de anuncios en este sitio" lo apaga para ese sitio, junto
  con lo que sigue salvo la prevención de rastreo del motor.
- **Ventanas emergentes.** Una ventana que la página abre sin que medie un
  clic o una tecla se bloquea siempre; una abierta con un clic se bloquea sólo
  si su dirección es un anuncio según las listas (reglas `$popup` o un
  servidor de anuncios entero: los popunders que se cuelgan del primer clic).
  Los logins con ventana (Google, Microsoft, Facebook, Apple, GitHub, PayPal)
  se abren: una prueba con las listas reales lo comprueba. Al bloquear, la
  página muestra "Ventana emergente bloqueada" y "Permitir ventanas emergentes
  en este sitio" (menú de la pestaña) las deja pasar; desde una pestaña
  privada ese permiso dura hasta cerrar Nea y no se guarda. Las páginas de
  extensiones no se tocan.
- **Rastreadores y cookies de terceros** (Ajustes → Privacidad, "Bloquear
  cookies de terceros y rastreadores", prendido de fábrica). La prevención de
  rastreo del motor en nivel *estricto*: medido, no deja cargar los scripts de
  Facebook, Google Analytics ni Hotjar desde un sitio ajeno (el nivel
  equilibrado, el de fábrica del motor, los dejaba pasar) y los logins de
  Google, Microsoft, GitHub y Amazon siguen cargando. Sus cookies y su
  almacenamiento quedan bloqueados con ellos; las cookies del sitio que se
  visita (las de la sesión) no se tocan. El motor no ofrece excepciones por
  sitio para este nivel, así que la excepción del bloqueo de anuncios no lo
  relaja. Las cookies de terceros que no son de rastreadores conocidos no se
  bloquean: WebView2 no tiene una opción soportada para eso (la única que
  funciona, `--test-third-party-cookie-phaseout`, es un interruptor de prueba
  de Chromium, no documentado para WebView2, y se dejó afuera).
- **Carteles de cookies.** Con el mismo interruptor: la EasyList Cookie List
  (de `secure.fanboy.co.nz`, bajada con las otras) oculta los avisos de
  consentimiento con sus reglas `##`, `dominio##` y `#@#` y corta, sitio por
  sitio, los scripts de gestores de consentimiento que la lista nombra. Cada
  documento le pide a Nea, al crearse, las reglas de su sitio (unas 180)
  y, a medida que aparecen, las genéricas de los `#id` y `.clase` que de
  verdad tiene, como hace uBlock: hasta el primer cuadro lo pregunta sin
  esperar, así que un cartel no llega a pintarse. Son pedidos que nunca salen
  de la máquina. Con las 15.000 reglas genéricas de entrada, cada pestaña
  gastaba ~10 MB más en su proceso; así, menos de 1 MB, y la página tarda
  ~15 ms más en estar lista. Si el cartel bloqueaba el desplazamiento, se
  libera. Los muros de "aceptá o pagá" (Spiegel, Corriere, The Guardian) la
  lista no los oculta a propósito. Compilada ocupa ~1,3 MB en memoria de
  Nea; apagada, no se compila.

## Datos locales

Sin cuentas, sin sincronización, sin telemetría y sin reporte de fallos. Lo
único que sale a la red son las páginas que se piden, sus íconos y la
comprobación diaria de actualización (que se puede apagar). El campo no envía
nada hasta presionar Enter: el autocompletado sale del historial local.

| Dato | Dónde |
|---|---|
| Ajustes, pestañas abiertas, historial, marcadores, descargas, elementos ocultos, extensiones | `%APPDATA%\Nea\*.json` |
| Listas de filtros (anuncios, rastreo, cookies) y extensiones de la Web Store | `%APPDATA%\Nea\shield`, `%APPDATA%\Nea\extensions` |
| Sitios con excepción de bloqueo o con ventanas emergentes permitidas | `settings.json` (`shield_disabled`, `popups_allowed`) |
| Contraseñas | Administrador de credenciales de Windows (genéricas `Nea:<origen> <usuario>`) |
| Íconos de los sitios (nunca los de pestañas privadas), hasta 4 MB | `%APPDATA%\Nea\favicons` |
| Cookies, caché y datos de sitios | `%LOCALAPPDATA%\Nea\engine` (perfil propio, separado de Edge) |
| Actualización descargada | `%LOCALAPPDATA%\Nea\update` |
| Programa instalado | `%LOCALAPPDATA%\Programs\Nea` |

Los JSON se escriben de forma atómica (archivo temporal y reemplazo), así que
un corte no deja un archivo a medias. Al desinstalar se borra el programa y la
actualización descargada; los datos quedan, por si se vuelve a instalar, y se
borran a mano eliminando esas carpetas.
