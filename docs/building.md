# Build, install and publish

## Requirements

- Windows 10 or 11 (x64).
- The WebView2 runtime (Evergreen). Windows 11 and almost all updated
  Windows 10 installations include it. If it is missing, the installer tells
  you and offers the
  [official download](https://go.microsoft.com/fwlink/p/?LinkId=2124703).

## Install

- **Installer:** `Nea-<version>-setup.exe`. It installs Nea for the current
  user in `%LOCALAPPDATA%\Programs\Nea`. It does not need administrator
  rights. It adds a shortcut to the Start menu and an entry to Settings →
  Apps. Use that entry to uninstall Nea.
- **Portable:** `Nea-<version>-portable.zip`. Extract it to any folder and
  open `nea.exe`. It does not change the registry.

`nea.exe` also accepts `--install` and `--uninstall`. Add `--quiet` to skip
all questions. Winget uses this mode.

## Build

You need these items:

- Stable Rust with the `x86_64-pc-windows-msvc` target.
- The Visual Studio build tools (MSVC) with the Windows SDK. The SDK supplies
  `rc.exe` for the icon and the manifest.

```powershell
cargo build            # development build: "Nea (development)"
cargo build --release  # the published version
cargo test
```

The development build shows its name in the window title, the shortcut and
Settings. It does not check for updates. The only exception is an explicit
`update_url` in `settings.json`.

`build.ps1` builds, signs and packages all files into `dist\`:

```powershell
.\build.ps1                       # release, not signed (shows a warning)
.\build.ps1 -Dev                  # development build
.\build.ps1 -CertificatePath cert.pfx -CertificatePassword ... `
            -UpdateKey C:\secrets\nea-update.key `
            -PackageUrl https://github.com/<owner>/nea/releases/download/v0.2.0/nea.exe
```

The script makes these files:

- `nea.exe`
- `Nea-<version>-setup.exe`
- `Nea-<version>-portable.zip`
- `latest.json` (only with `-UpdateKey`)
- `SHA256SUMS.txt`

The zip file has a fixed file order and fixed dates (from the last commit).
Thus the zip file is the same in each run if the program does not change.
Without a code signing certificate, the script shows a warning and continues.
The program works, but SmartScreen shows a warning when you open it.

`packaging/winget/` contains a sample winget manifest. It is not published.

## Script control

Script control is for tests and measurements. It is off by default. To turn
it on, use Settings → Testing or start Nea with `NEA_BENCH=1`. It listens
only on a local *named pipe*. Only the current user can connect to it. The
tabs that it opens are test tabs. Nea does not add them to the history, the
session or Ctrl+Shift+T.

```powershell
$env:NEA_DATA_DIR = "$env:TEMP\nea-test"   # separate data
$env:NEA_BENCH = "1"
$env:NEA_BENCH_PIPE = "test"   # a different channel from the daily one
.\target\release\nea.exe

cargo run --release --example bench -- open https://example.com/
cargo run --release --example bench -- wait
cargo run --release --example bench -- text
cargo run --release --example bench -- run new_tab   # any command by its name
cargo run --release --example bench -- type "example.com" --submit
cargo run --release --example bench -- state         # tabs, field, pairs, private tabs, groups
cargo run --release --example bench -- shield        # blocks, rules, pop-ups and cookie list
cargo run --release --example bench -- quit
```

`tools\bench.ps1` does the same from PowerShell. `tools\measure.ps1` runs the
memory scenarios. `NEA_CREDENTIAL_PREFIX` (for example `NeaTest:`) keeps the
passwords of a test run separate from the real passwords. When a different
application is in front, Nea does not take the keyboard. Thus a script run
does not interrupt a person who types in a different window.

## Diagnostics: crashes and hangs

Diagnostics are always on and have almost no cost. A thread checks once a
second that the window still processes messages. No data leaves the
computer. The data goes into text files in the data folder (`%APPDATA%\Nea`,
or `NEA_DATA_DIR`). Each file is 256 KB at most. Nea keeps the previous file
as `*.log.old`.

| File | Contents |
|---|---|
| `crash.log` | A *panic*: the message, file, line, time, version and the current task of the window. An unhandled exception: the code and the address in `nea.exe`. Each engine process that crashes or stops responding (browser, page, GPU…), with its reason and exit code. |
| `hang.log` | Each time the window stops for more than 2 s: the start time and the current task (an engine event, a command, a timer, the test pipe, a blocker request…). It also records when the window responds again, and each step that takes more than 250 ms. |

If Nea closes or stops responding, these two files show the cause. If the
full engine process crashes, Nea starts it again and reloads the visible tabs.
It does this up to three times in two minutes. If the GPU or a helper process
crashes, the engine recovers and the tabs do not change.

Nea prepares a spare view before it needs one, so that Enter does not wait
for the engine. It prepares the view in these cases:

- When the field opens for a new tab.
- While you type in the field.
- When the pointer is on a sleeping tab.
- When you switch tabs and a sleeping tab exists.

With an empty window, the engine starts at the first key press, not when the
window opens. If nobody uses the spare view in 30 s, Nea closes it. If the
engine started only for that view, the engine also stops. Thus an idle
window uses no extra memory.

For measurements, `NEA_TRACE=1` adds `trace.log`. It records a time (in ms
from startup) for each step between Enter and the page. It also records each
handler that takes more than 4 ms. `tools\latency.ps1` uses this file to
measure the latency of the field: cold, warm and with a sleeping tab. With
`-Usage`, it does a heavy-use run with 30 real tabs and a history of 20,000
entries. Then it shows the slow handlers and the contents of `hang.log`.

## Updates

Once a day, Nea reads a manifest (`latest.json`) in the background. If the
manifest shows a higher version, Nea downloads the package to
`%LOCALAPPDATA%\Nea\update`. Then it verifies the Ed25519 signature with the
public key in the program. If the signature is not correct, Nea deletes the
package.

Nea never restarts automatically. The next time Nea starts, it replaces the
executable before it shows the window. It keeps the previous executable as
`nea.old` and deletes it at the next start.

If the manifest does not respond, Nea tries again the next day. To turn off
the check, use Settings → Updates. That page also shows the installed
version.

### Sign a release

The signature includes the version and the bytes of the package. Thus nobody
can supply an old package as a new one.

1. Make the production key pair one time, outside the repository:

   ```powershell
   cargo run --example keygen -- C:\secrets\nea-update.key keys\update-public.hex
   ```

   Keep the private key in a secrets manager. Never commit it. `.gitignore`
   contains `keys/*.key`. Commit the public key (`keys/update-public.hex`),
   because the build puts it into the program. The current key in the
   repository is a **development** key. Replace it before the first public
   release.
2. The updater reads `latest.json` from the latest release of
   `github.com/Agustin-Delgado/nea` (`RELEASES_OWNER` in `src/updater.rs`).
3. Run `.\build.ps1 -UpdateKey <private key> -PackageUrl <URL of nea.exe>`.
   Then upload `nea.exe` and `latest.json` to the release.

To sign manually, use `cargo run --example sign -- <package> <private key> [--version x.y.z] [--url url] [--out latest.json]`.
