# Liviano

Un navegador para Windows pequeño, rápido y silencioso: una fila de pestañas,
un campo y la página. Sin barra de herramientas, sin página de inicio, sin
cuentas y sin telemetría. Todo pasa por un único campo que sirve para
direcciones y búsquedas.

Está escrito en Rust con interfaz Win32 nativa y usa el motor WebView2 que ya
trae Windows. La prioridad número uno es gastar poca memoria: el programa
propio ocupa unos pocos MB, las pestañas de fondo se duermen y, pasado un
rato, se descartan conservando sólo su dirección y su título.

Las ideas vienen del navegador [Search](https://github.com/driceroland/Search)
de driceroland (licencia MIT), que sirvió de referencia funcional. Liviano no
contiene su código, su nombre ni su ícono.

## Cómo se ve

La interfaz se dibuja con Direct2D y DirectWrite sobre ventanas Win32, con
los grises planos de Search, esquinas redondeadas y barra de título oscura de
Windows 11 (sin Mica, que tiñe con el fondo de pantalla). Sin interfaz web.

| | |
|---|---|
| ![Pestañas arriba, tema claro](docs/capturas/claro-horizontal.png) | ![Pestañas al costado, tema oscuro](docs/capturas/oscuro-vertical.png) |
| ![Campo único abierto](docs/capturas/campo-claro.png) | ![Historial, tema oscuro](docs/capturas/historial-oscuro.png) |
| ![Ajustes, tema claro](docs/capturas/ajustes-claro.png) | ![Ajustes, tema oscuro](docs/capturas/ajustes-oscuro.png) |

Las capturas salen de `tools/capture.ps1` (PrintWindow, sin traer la ventana
al frente); la sombra y el borde que dibuja Windows no aparecen en ellas.

## Qué hace

Estado de cada función de la especificación (sección 3) en esta versión.
"Verificado" quiere decir probado con el programa compilado, manejado por el
canal de control por script; lo que no se puede probar así está marcado.

| | Función | Estado |
|---|---|---|
| RF-01 | Campo único para direcciones y búsquedas | Verificado. Nada sale hasta Enter. |
| RF-02 | Autocompletado desde el historial local | Verificado (sugerencias y completado en línea). |
| RF-03 | Pestañas arriba o a la izquierda, barra lateral plegable | Verificado. |
| RF-04 | Pestañas fijadas | Verificado; persisten entre sesiones. |
| RF-05 | Restauración diferida | Verificado: al abrir sólo se crea la vista de la pestaña activa (y la de su par, si está dividida). |
| RF-06 | Cambiar de pestaña por nombre | Verificado (búsqueda difusa por título y dirección). |
| RF-07 | Dormir y descartar pestañas de fondo | Verificado: se suspenden pasado el tiempo de Ajustes y se descartan después, o enseguida con poca memoria. |
| RF-08 | Mantener un sitio despierto | Verificado. |
| RF-09 | Vista dividida | Verificado (pares, persistencia, privadas). El arrastre de pestañas y del divisor se revisa a mano. |
| RF-10 | Modo lectura | Verificado (Readability). |
| RF-11 | Ocultar elementos por sitio | Verificado (selección por clic, se aplica antes de pintar). La lista de ocultos (Ctrl+Shift+U) es un menú: se revisa a mano. |
| RF-12 | Video flotante | Hecho con el Picture-in-Picture del motor. Sin verificar en esta pasada (hace falta una página con video). |
| RF-13 | Bloqueo de anuncios y trackers | Verificado, con excepción por sitio (menú de la pestaña). Usa EasyList y EasyPrivacy, con una lista mínima incluida mientras se descargan. |
| RF-14 | Contraseñas en el Administrador de credenciales | Verificado: ofrece guardar tras un login y nunca rellena solo. Ctrl+Alt+L abre el panel. |
| RF-15 | Importar contraseñas de otros navegadores | Hecho (Chrome, Edge, Brave, Vivaldi, Opera, Arc y CSV, todo en el equipo). Sin verificar en esta pasada: las pruebas no leen perfiles reales. |
| RF-16 | Llaves de acceso | El motor deriva WebAuthn al diálogo de Windows Hello. Sin verificar en esta pasada. |
| RF-17 | Tema claro, oscuro o del sistema | Verificado claro y oscuro; "del sistema" sigue el cambio de Windows. |
| RF-18 | Marcadores, historial y descargas | Verificado. |
| RF-19 | Pestañas privadas | Verificado: perfil InPrivate propio; nada va al historial, a la sesión, a Ctrl+Shift+T ni a settings.json. |
| RF-20 | Extensiones de Chrome | Verificado: carga, botón (sólo si hay extensiones) y fijadas. Limitaciones en [docs/extensiones.md](docs/extensiones.md). |
| RF-21 | Buscar en la página | Verificado. |
| RF-22 | Actualización diaria firmada | Hecho, con pruebas unitarias. **Apagado** hasta definir el dueño del repositorio de versiones (ver al final). |
| RF-23 | Control por script | Verificado (ver "Control por script"). |
| RF-24 | Ajustes | Verificado: general, pestañas, contraseñas, extensiones, atajos, actualizaciones y pruebas; todo se guarda al momento. |

La memoria medida contra Edge está en [docs/medicion.md](docs/medicion.md).

## Requisitos

- Windows 10 u 11 (x64).
- El runtime de WebView2 (Evergreen). Viene con Windows 11 y con casi todos los
  Windows 10 actualizados. Si falta, el instalador avisa y ofrece la
  [descarga oficial](https://go.microsoft.com/fwlink/p/?LinkId=2124703).

## Instalar

- **Instalador:** `Liviano-<versión>-setup.exe`. Se instala para el usuario
  actual, sin permisos de administrador, en `%LOCALAPPDATA%\Programs\Liviano`,
  con acceso directo en el menú Inicio y entrada en Configuración → Aplicaciones
  (desde ahí se desinstala).
- **Portable:** `Liviano-<versión>-portable.zip`. Se descomprime donde sea y se
  abre `liviano.exe`; no toca el registro.

El mismo `liviano.exe` entiende `--install` y `--uninstall` (más `--quiet` para
hacerlo sin preguntas, como lo usa winget).

## Atajos

Todos se pueden cambiar en Ajustes (Ctrl+,).

| Acción | Atajo |
|---|---|
| Barra de direcciones | Ctrl+L |
| Cambiar de pestaña por nombre | Ctrl+K |
| Nueva / cerrar / reabrir pestaña | Ctrl+T / Ctrl+W / Ctrl+Shift+T |
| Nueva pestaña privada | Ctrl+Shift+N |
| Atrás / adelante | Alt+Izquierda / Alt+Derecha |
| Pestaña anterior / siguiente | Ctrl+Shift+Tab / Ctrl+Tab (y Ctrl+RePág / Ctrl+AvPág) |
| Ir a la pestaña 1 a 8 / a la última | Ctrl+1 … Ctrl+8 / Ctrl+9 |
| Pestañas arriba / a la izquierda | Ctrl+Shift+S |
| Plegar barra lateral | Ctrl+S |
| Buscar en la página | Ctrl+F |
| Duplicar pestaña | Ctrl+D |
| Copiar dirección / pegar e ir | Ctrl+Shift+C / Ctrl+Shift+V |
| Recargar / recargar sin caché | F5 o Ctrl+R / Ctrl+F5 |
| Marcar página | Ctrl+Shift+B |
| Marcadores / historial / descargas | Ctrl+Shift+O / Ctrl+Y / Ctrl+Shift+J |
| Modo lectura | Ctrl+Shift+R |
| Video flotante | Ctrl+Shift+P |
| Ocultar elemento / ver ocultos | Ctrl+Shift+H / Ctrl+Shift+U |
| Vista dividida / deshacerla | Ctrl+Alt+N / Ctrl+Alt+Shift+N |
| Ir a la página dividida de la izquierda / derecha | Alt+Shift+Izquierda / Alt+Shift+Derecha |
| Contraseñas | Ctrl+Alt+L |
| Ajustes | Ctrl+, |

Fijar una pestaña, mantener un sitio despierto y el bloqueo de anuncios en un
sitio están en el menú de la pestaña (clic derecho) y no tienen atajo de
fábrica; se les puede asignar uno en Ajustes.

## Compilar

Hace falta Rust estable con el destino `x86_64-pc-windows-msvc` y las
herramientas de compilación de Visual Studio (MSVC).

```powershell
cargo build            # desarrollo: se llama "Liviano (desarrollo)"
cargo build --release  # la versión que se publica
```

Los tests viven en un repositorio privado.

La compilación de desarrollo se distingue en el título de la ventana, en el
acceso directo y en Ajustes, y no busca actualizaciones salvo que
`settings.json` tenga una dirección explícita (`update_url`).

`build.ps1` compila, firma y empaqueta todo en `dist\`:

```powershell
.\build.ps1                       # release, sin firmar (avisa)
.\build.ps1 -Dev                  # variante de desarrollo
.\build.ps1 -CertificatePath cert.pfx -CertificatePassword ... `
            -UpdateKey C:\secretos\liviano-update.key `
            -PackageUrl https://github.com/<owner>/liviano/releases/download/v0.2.0/liviano.exe
```

Deja `liviano.exe`, `Liviano-<versión>-setup.exe`, `Liviano-<versión>-portable.zip`,
`latest.json` (si se pasó `-UpdateKey`) y `SHA256SUMS.txt`. El zip se arma
con orden y fechas fijos (las del último commit), así que no cambia entre
corridas si no cambia el programa. Sin certificado de firma de código el
script avisa y sigue: el programa funciona, pero SmartScreen advierte al abrirlo.

En `packaging/winget/` hay un manifiesto de winget de ejemplo (sin publicar).

## Control por script

Para pruebas y medición (RF-23). Está apagado: se prende en Ajustes →
Pruebas o arrancando con `LIVIANO_BENCH=1`. Escucha sólo en un *named pipe*
local, accesible únicamente para el usuario actual. Las pestañas que abre son
"de prueba": no van al historial, a la sesión ni a Ctrl+Shift+T.

```powershell
$env:LIVIANO_DATA_DIR = "$env:TEMP\liviano-prueba"   # datos aparte
$env:LIVIANO_BENCH = "1"
$env:LIVIANO_BENCH_PIPE = "prueba"   # otro canal, sin chocar con el de uso diario
.\target\release\liviano.exe

cargo run --release --example bench -- open https://example.com/
cargo run --release --example bench -- wait
cargo run --release --example bench -- text
cargo run --release --example bench -- run new_tab   # cualquier comando por su nombre
cargo run --release --example bench -- type "example.com" --submit
cargo run --release --example bench -- state         # pestañas, campo, pares, privadas
cargo run --release --example bench -- quit
```

`tools\bench.ps1` hace lo mismo desde PowerShell y `tools\measure.ps1` corre
los escenarios de memoria. `LIVIANO_CREDENTIAL_PREFIX` (por ejemplo
`LivianoTest:`) separa las contraseñas de una corrida de prueba de las reales.
Con otra aplicación al frente, Liviano no toma el teclado: una corrida por
script no interrumpe a quien escribe en otra ventana.

## Actualizaciones

Una vez por día, en segundo plano, Liviano lee un manifiesto (`latest.json`).
Si anuncia una versión mayor, descarga el paquete a
`%LOCALAPPDATA%\Liviano\update` y verifica su firma Ed25519 contra la clave
pública que viene dentro del programa. Si la firma no verifica, lo descarta.
Nunca reinicia por su cuenta: la próxima vez que se abre Liviano, antes de
mostrar la ventana, reemplaza el ejecutable (el anterior queda como
`liviano.old` y se borra en el arranque siguiente) y sigue con la versión nueva.

Si el manifiesto no responde, no pasa nada y se vuelve a intentar al día
siguiente. La comprobación se apaga en Ajustes → Actualizaciones, donde también
se ve la versión instalada.

### Firmar una versión

La firma cubre la versión y los bytes del paquete, así que un paquete viejo no
se puede servir como si fuera nuevo.

1. Generar el par de claves de producción, una sola vez, fuera del repositorio:

   ```powershell
   cargo run --example keygen -- C:\secretos\liviano-update.key keys\update-public.hex
   ```

   La clave privada se guarda en un gestor de secretos y no se commitea nunca
   (`keys/*.key` está en `.gitignore`). La pública (`keys/update-public.hex`)
   sí se commitea: es la que se compila dentro del programa. La que está hoy
   en el repo es una clave de **desarrollo** y hay que reemplazarla antes de
   la primera versión pública.
2. El actualizador lee `latest.json` del último release de
   `github.com/Agustin-Delgado/liviano` (`RELEASES_OWNER` en `src/updater.rs`).
3. `.\build.ps1 -UpdateKey <clave privada> -PackageUrl <dirección de liviano.exe>`
   y subir `liviano.exe` y `latest.json` al release.

Firmar a mano: `cargo run --example sign -- <paquete> <clave privada> [--version x.y.z] [--url dirección] [--out latest.json]`.

## Datos y privacidad

Sin cuentas, sin sincronización, sin telemetría y sin reporte de fallos. Lo
único que sale a la red son las páginas que se piden, sus íconos y la
comprobación diaria de actualización (que se puede apagar). El campo no envía
nada hasta presionar Enter: el autocompletado sale del historial local.

| Dato | Dónde |
|---|---|
| Ajustes, pestañas abiertas, historial, marcadores, descargas, elementos ocultos, extensiones | `%APPDATA%\Liviano\*.json` |
| Listas de filtros y extensiones de la Web Store | `%APPDATA%\Liviano\shield`, `%APPDATA%\Liviano\extensions` |
| Contraseñas | Administrador de credenciales de Windows (genéricas `Liviano:<origen> <usuario>`) |
| Cookies, caché y datos de sitios | `%LOCALAPPDATA%\Liviano\engine` (perfil propio, separado de Edge) |
| Actualización descargada | `%LOCALAPPDATA%\Liviano\update` |
| Programa instalado | `%LOCALAPPDATA%\Programs\Liviano` |

Los JSON se escriben de forma atómica (archivo temporal y reemplazo), así que
un corte no deja un archivo a medias. Al desinstalar se borra el programa y la
actualización descargada; los datos quedan, por si se vuelve a instalar, y se
borran a mano eliminando esas carpetas.

## Lo que falta y requiere a una persona

El código está completo; estas decisiones y secretos no pueden salir de él:

- **Primer release.** El actualizador ya apunta a los releases de
  `github.com/Agustin-Delgado/liviano`; mientras no haya ninguno, la
  comprobación diaria no encuentra nada y no hace nada.
- **Clave de firma de producción.** La pública de `keys/update-public.hex` es
  de desarrollo. Hay que generar el par de producción (ver "Firmar una
  versión"), guardar la privada en un gestor de secretos y commitear sólo la
  pública.
- **Certificado de firma de código.** Sin él, SmartScreen advierte al abrir
  el instalador y el programa. `build.ps1 -CertificatePath` lo usa cuando
  existe.
- **Correo de seguridad.** `SECURITY.md` tiene `<correo-de-seguridad>` como
  marcador.
- **winget.** El manifiesto de `packaging/winget/` es un ejemplo sin
  publicar; necesita la dirección y el hash del primer release.

Se revisan a mano: arrastrar pestañas y el divisor de la vista dividida, los
menús (pestaña, extensiones, elementos ocultos, cuentas de un login), el video
flotante, la importación de contraseñas y las llaves de acceso.

## Seguridad

Para reportar una vulnerabilidad, ver [SECURITY.md](SECURITY.md). Por favor no
abras un issue público.

## Licencia

MIT (ver [LICENSE](LICENSE)). Componentes de terceros en [NOTICE](NOTICE).
