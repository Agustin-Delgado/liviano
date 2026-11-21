# Compilar, instalar y publicar

## Requisitos para usarlo

- Windows 10 u 11 (x64).
- El runtime de WebView2 (Evergreen). Viene con Windows 11 y con casi todos los
  Windows 10 actualizados. Si falta, el instalador avisa y ofrece la
  [descarga oficial](https://go.microsoft.com/fwlink/p/?LinkId=2124703).

## Instalar

- **Instalador:** `Nea-<versión>-setup.exe`. Se instala para el usuario
  actual, sin permisos de administrador, en `%LOCALAPPDATA%\Programs\Nea`,
  con acceso directo en el menú Inicio y entrada en Configuración → Aplicaciones
  (desde ahí se desinstala).
- **Portable:** `Nea-<versión>-portable.zip`. Se descomprime donde sea y se
  abre `nea.exe`; no toca el registro.

El mismo `nea.exe` entiende `--install` y `--uninstall` (más `--quiet` para
hacerlo sin preguntas, como lo usa winget).

## Compilar

Hace falta Rust estable con el destino `x86_64-pc-windows-msvc` y las
herramientas de compilación de Visual Studio (MSVC) con el Windows SDK, que
trae `rc.exe` para el ícono y el manifiesto.

```powershell
cargo build            # desarrollo: se llama "Nea (desarrollo)"
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
            -UpdateKey C:\secretos\nea-update.key `
            -PackageUrl https://github.com/<owner>/nea/releases/download/v0.2.0/nea.exe
```

Deja `nea.exe`, `Nea-<versión>-setup.exe`, `Nea-<versión>-portable.zip`,
`latest.json` (si se pasó `-UpdateKey`) y `SHA256SUMS.txt`. El zip se arma
con orden y fechas fijos (las del último commit), así que no cambia entre
corridas si no cambia el programa. Sin certificado de firma de código el
script avisa y sigue: el programa funciona, pero SmartScreen advierte al abrirlo.

En `packaging/winget/` hay un manifiesto de winget de ejemplo (sin publicar).

## Control por script

Para pruebas y medición. Está apagado: se prende en Ajustes →
Pruebas o arrancando con `NEA_BENCH=1`. Escucha sólo en un *named pipe*
local, accesible únicamente para el usuario actual. Las pestañas que abre son
"de prueba": no van al historial, a la sesión ni a Ctrl+Shift+T.

```powershell
$env:NEA_DATA_DIR = "$env:TEMP\nea-prueba"   # datos aparte
$env:NEA_BENCH = "1"
$env:NEA_BENCH_PIPE = "prueba"   # otro canal, sin chocar con el de uso diario
.\target\release\nea.exe

cargo run --release --example bench -- open https://example.com/
cargo run --release --example bench -- wait
cargo run --release --example bench -- text
cargo run --release --example bench -- run new_tab   # cualquier comando por su nombre
cargo run --release --example bench -- type "example.com" --submit
cargo run --release --example bench -- state         # pestañas, campo, pares, privadas, grupos
cargo run --release --example bench -- shield        # bloqueos, reglas, popups y lista de cookies
cargo run --release --example bench -- quit
```

`tools\bench.ps1` hace lo mismo desde PowerShell y `tools\measure.ps1` corre
los escenarios de memoria. `NEA_CREDENTIAL_PREFIX` (por ejemplo
`NeaTest:`) separa las contraseñas de una corrida de prueba de las reales.
Con otra aplicación al frente, Nea no toma el teclado: una corrida por
script no interrumpe a quien escribe en otra ventana.

## Diagnóstico: cierres y cuelgues

Siempre activo y sin costo apreciable (un hilo que mira una vez por segundo
si la ventana sigue procesando mensajes). Nada sale de la máquina: son
archivos de texto en la carpeta de datos (`%APPDATA%\Nea`, o
`NEA_DATA_DIR`), de 256 KB como máximo cada uno (el anterior queda como
`*.log.old`).

| Archivo | Qué anota |
|---|---|
| `crash.log` | Un *panic* (mensaje, archivo y línea, hora, versión y qué estaba haciendo la ventana), una excepción no manejada (código y dirección dentro de `nea.exe`) y cada proceso del motor que se cae o deja de responder: el del navegador, el de una página, la GPU… con su motivo y código de salida. |
| `hang.log` | La ventana trabada más de 2 s: desde cuándo y haciendo qué (un evento del motor, un comando, un temporizador, el pipe de pruebas, una solicitud del bloqueador…), y cuándo volvió a responder. También cualquier paso suelto que tardó más de 250 ms. |

Si Nea se cierra solo o se queda "sin responder", esos dos archivos
dicen dónde. Cuando se cae el proceso del motor entero, Nea lo vuelve a
arrancar y recarga las pestañas visibles (hasta tres veces en dos minutos);
si se cae la GPU o un proceso auxiliar, el motor se recupera solo y las
pestañas no se tocan.

Para que Enter no espere al motor, Nea prepara una vista de reserva
cuando está por necesitarla: al abrir el campo de pestaña nueva, mientras se
escribe en él, con el puntero sobre una pestaña dormida o al cambiar de
pestaña habiendo alguna dormida. Con la ventana vacía, el motor arranca con
la primera tecla y no al abrirla. Si nadie usa la vista en 30 s se cierra, y
si el motor se había arrancado sólo para ella también se apaga: una ventana
quieta no guarda nada de más.

Para medir, `NEA_TRACE=1` agrega `trace.log`: una marca con hora (en ms
desde el arranque) por cada paso entre Enter y la página, y cada manejador
que tardó más de 4 ms. `tools\latency.ps1` lo usa para medir la latencia del
campo en frío, en caliente y con una pestaña dormida, y con `-Usage` una
pasada de uso pesado (30 pestañas reales, historial de 20 000 entradas) que
resume los manejadores lentos y lo que haya quedado en `hang.log`.

## Actualizaciones

Una vez por día, en segundo plano, Nea lee un manifiesto (`latest.json`).
Si anuncia una versión mayor, descarga el paquete a
`%LOCALAPPDATA%\Nea\update` y verifica su firma Ed25519 contra la clave
pública que viene dentro del programa. Si la firma no verifica, lo descarta.
Nunca reinicia por su cuenta: la próxima vez que se abre Nea, antes de
mostrar la ventana, reemplaza el ejecutable (el anterior queda como
`nea.old` y se borra en el arranque siguiente) y sigue con la versión nueva.

Si el manifiesto no responde, no pasa nada y se vuelve a intentar al día
siguiente. La comprobación se apaga en Ajustes → Actualizaciones, donde también
se ve la versión instalada.

### Firmar una versión

La firma cubre la versión y los bytes del paquete, así que un paquete viejo no
se puede servir como si fuera nuevo.

1. Generar el par de claves de producción, una sola vez, fuera del repositorio:

   ```powershell
   cargo run --example keygen -- C:\secretos\nea-update.key keys\update-public.hex
   ```

   La clave privada se guarda en un gestor de secretos y no se commitea nunca
   (`keys/*.key` está en `.gitignore`). La pública (`keys/update-public.hex`)
   sí se commitea: es la que se compila dentro del programa. La que está hoy
   en el repo es una clave de **desarrollo** y hay que reemplazarla antes de
   la primera versión pública.
2. El actualizador lee `latest.json` del último release de
   `github.com/Agustin-Delgado/nea` (`RELEASES_OWNER` en `src/updater.rs`).
3. `.\build.ps1 -UpdateKey <clave privada> -PackageUrl <dirección de nea.exe>`
   y subir `nea.exe` y `latest.json` al release.

Firmar a mano: `cargo run --example sign -- <paquete> <clave privada> [--version x.y.z] [--url dirección] [--out latest.json]`.
