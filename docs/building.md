# Build, install and publish

## Requirements

- Windows 10 or 11 (x64).
- The WebView2 runtime (Evergreen). Windows 11 and almost all updated
  Windows 10 installations include it. If it is missing, the installer tells
  you and offers the
  [official download](https://go.microsoft.com/fwlink/p/?LinkId=2124703).

## Install

Download the files from the
[latest release](https://github.com/Agustin-Delgado/nea/releases/latest).

- **Program:** `nea.exe`. Run it. It runs from where it is, as a portable
  app.
- **Installer:** `Nea-<version>-setup.exe`. It installs Nea for the current
  user in `%LOCALAPPDATA%\Programs\Nea`. It does not need administrator
  rights. It adds a shortcut to the Start menu and an entry to Settings →
  Apps. Use that entry to uninstall Nea.
- **Portable:** `Nea-<version>-portable.zip`. Extract it to any folder and
  open `nea.exe`. It does not change the registry.

`nea.exe` also accepts `--install` and `--uninstall`. Add `--quiet` to skip
all questions. Winget uses this mode.

The releases are not signed with a code signing certificate. When you open
`nea.exe` or the installer for the first time, SmartScreen can show "Windows
protected your PC". Select **More info**, then **Run anyway**. Compare the
SHA-256 hash of the file with `SHA256SUMS.txt` in the release.

## Build

You need these items:

- [rustup](https://rustup.rs). `rust-toolchain.toml` pins the Rust version
  and the components. CI uses the same file.
- The Visual Studio build tools (MSVC) with the Windows SDK. The SDK supplies
  `rc.exe` for the icon and the manifest.

```powershell
cargo build            # development build: "Nea (development)"
cargo build --release  # the published version
```

## Tests

The tests are in a private repository, `Agustin-Delgado/nea-tests`. This
repository mounts it as the git submodule `tests/`. Pull requests are tested
by CI. If you have access to the tests, clone with `--recurse-submodules`:

```powershell
git clone --recurse-submodules https://github.com/Agustin-Delgado/nea.git
cd nea
cargo test
```

In a clone without the submodule, run `git submodule update --init tests`.

How the parts connect:

- Each module of `src` that has tests ends with a hook. The hook loads the
  tests of the module from the submodule:

  ```rust
  #[cfg(all(test, nea_tests))]
  #[path = "../tests/unit/omnibox.rs"]
  #[rustfmt::skip]
  mod tests;
  ```

  The `#[path]` is relative to the folder of the source file.
- `build.rs` sets the `nea_tests` cfg only when `tests/README.md` exists.
  Without the submodule, the hooks are off. `cargo build`, `cargo clippy` and
  `cargo test` work, and `cargo test` runs 0 tests.
- `#[rustfmt::skip]` stops `cargo fmt` from opening the missing files. CI
  checks the format of the test files with `rustfmt` directly.
- `autotests = false` in `Cargo.toml` stops Cargo from building the files in
  `tests/` as integration tests.

Some tests use the real Windows Credential Manager. Each test writes only
under its own prefix (`NeaTest-…`), unique to the test and to the process,
and deletes its entries when it ends.

To change a test, commit in `tests/` and push to `nea-tests`. Then commit the
new submodule pointer here.

## Continuous integration

`.github/workflows/ci.yml` runs on each push to `main`, on each pull request
and on a manual run, on `windows-latest`:

1. It mounts the tests with the deploy key in the secret `TESTS_DEPLOY_KEY`.
   If the secret is not available (a pull request from a fork), it shows a
   warning and continues without the tests.
2. It installs the toolchain of `rust-toolchain.toml`.
3. It runs `cargo fmt --all --check`, and `rustfmt --check` on the test files.
4. It runs `cargo clippy -- -D warnings`, with `--all-targets` when the tests
   are mounted.
5. It runs `cargo build --release` and, when the tests are mounted,
   `cargo test`.

The development build shows its name in the window title, the shortcut and
Settings. It does not check for updates. The only exception is an explicit
`update_url` in `settings.json`. A build that runs from a cargo target folder
never downloads or installs an update (refer to [Updates](#updates)).

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

`packaging/winget/` contains the winget manifest of the last version that was sent to
[microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs). For a new version, copy the folder, change the version, the URL and `InstallerSha256` (the hash of `Nea-<version>-setup.exe`), run `winget validate --manifest <folder>` and open a pull request there.

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
cargo run --release --example bench -- menu open --x 60 --y 120   # a right click on the window
cargo run --release --example bench -- menu key --key down         # up, down, home, end, left, right, enter, escape, a letter
cargo run --release --example bench -- menu state    # the open levels, their items, the time to the first frame
cargo run --release --example bench -- menu close
cargo run --release --example bench -- quit
```

The `menu` requests also run while a menu is open, when all other requests
wait: `open` (with `--left` for a left click), `state`, `key`, `hover` and
`click` (an item, with `--level` and `--item`) and `close`. `tools\menus.ps1`
uses them: it measures the memory of the app before and after many menus,
measures the time from the click to the first frame, and saves captures of
the menus in the light and the dark theme.

`tools\bench.ps1` does the same from PowerShell. `tools\measure.ps1` runs the
memory scenarios. `NEA_CREDENTIAL_PREFIX` (for example `NeaTest:`) keeps the
passwords of a test run separate from the real passwords. When a different
application is in front, Nea does not take the keyboard. Thus a script run
does not interrupt a person who types in a different window. To measure what
Nea does in front (for example the focus that goes to the page after a tab
closes) without taking the foreground, also set `NEA_BENCH_FRONT=1`: with
`NEA_BENCH=1`, Nea then acts as if it were in front.

## Diagnostics: crashes and hangs

Diagnostics are always on and have almost no cost. A thread checks once a
second that the window still processes messages. No data leaves the
computer. The data goes into text files in the data folder (`%APPDATA%\Nea`,
or `NEA_DATA_DIR`). Each file is 256 KB at most. Nea keeps the previous file
as `*.log.old`.

| File | Contents |
|---|---|
| `crash.log` | A *panic*: the message, file, line, time, version and the current task of the window. An unhandled exception: the code, the address as `module+0xoffset` and the stack (up to 24 frames). Each engine process that crashes or stops responding (browser, page, GPU…), with its reason and exit code. |
| `hang.log` | Each time the window stops for more than 2 s: the start time and the current task (an engine event, a command, a timer, the test pipe, a blocker request…). It also records when the window responds again, and each step that takes more than 250 ms. |

If Nea closes or stops responding, these two files show the cause. If the
full engine process crashes, Nea starts it again and reloads the visible tabs.
It does this up to three times in two minutes. After that, it waits for the
end of the two minutes, then it starts the engine again. If the GPU or a helper process
crashes, the engine recovers and the tabs do not change. If WebView2 is not
installed or is updating, Nea tries again after 1 s, 2 s, 4 s… (at most each
minute), and the tabs load when the engine is available.

### Read a crash.log

The frames in `crash.log` are offsets, such as `nea.exe+0x1A2B3C`: the
release build has no names in it. Each release publishes the symbols of its
`nea.exe` as `nea-<version>.pdb`, next to the other files. To get the
function, the file and the line of each frame:

1. Find the version in the line of the crash (`panic v0.1.3`, `exception
   v0.1.3`), and download `nea-<version>.pdb` of that release. Only the PDB
   of the same build gives correct names.
2. Run:

   ```powershell
   tools\symbolize.ps1 -Pdb nea-0.1.3.pdb -Log crash.log
   ```

   Each line that has `nea.exe+0x…` gets its function, file and line at the
   end. For one offset: `tools\symbolize.ps1 -Pdb nea-0.1.3.pdb -Offset 0x1A2B3C`.
   Frames in other modules (`ntdll.dll`, `EmbeddedBrowserWebView.dll`) stay as
   they are: their symbols come from Microsoft.

The script uses `dbghelp.dll` of Windows. It needs no other tool and no
network. For your own build, use `target\release\nea.pdb`.

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

Nea reads a manifest (`latest.json`) in the background, on its own thread.
The first check is 20 seconds after Nea starts. Then Nea checks every 4
hours while it runs. If a check ran less than 10 minutes before the start,
the start does not check again. Each check sends the `ETag` of the last
manifest. If the manifest did not change, the server sends 304 and no body.

If the manifest shows a higher version, Nea downloads the package to
`%LOCALAPPDATA%\Nea\update`. Then it verifies the Ed25519 signature with the
public key in the program. If the signature is not correct, Nea deletes the
package.

Nea never restarts automatically. When a verified package is ready, a small
**Update** chip shows next to the gear (left of the zoom chip). Its tooltip
shows the version, for example "Restart to update to 0.1.8". The menu of
the tab strip (right-click an empty area) has the same item. A click does
these steps:

1. Nea closes the window as a normal close does. It writes the session and
   closes the pages.
2. Nea starts its executable again with the same command-line arguments.
   The new process gets the ID of the old process in `NEA_RESTART_WAIT`. It
   waits until the old process stops (20 seconds at most), because the old
   process holds the profile and the session file until it stops.
3. The new process replaces the executable before it shows the window, and
   starts the new version. The new version opens the saved tabs again.
   Private tabs do not come back, as after a normal close.

If you do not click the chip, the next start installs the update in the same
way. Nea keeps the previous executable as `nea.old` and deletes it at the
next start.

If the manifest does not respond, Nea tries again at the next check. Nea
writes the result of the last check to `update.json` in the data folder
(`last_result`). To turn off the check, use Settings → Updates. That page
also shows the installed version.

A build that runs from a cargo target folder never downloads a package and
never replaces its executable. Nea finds such a build from its path: a
`target\release` or `target\debug` folder with a `Cargo.toml` above it, or a
folder that has the cargo folders `deps` and `.fingerprint` next to the
executable. Then `last_result` tells the reason.

To test the update flow, use a temporary data folder (`NEA_DATA_DIR`), a
copy of a release build outside the target folder, and an `update_url` in
`settings.json` that points to a server on `127.0.0.1`. Plain `http` is
permitted only for this computer. Sign the test package with a test key, and
build the test copy with the public half of that key in
`keys/update-public.hex`. Do not commit that change.

### Sign a release

The signature includes the version and the bytes of the package. Thus nobody
can supply an old package as a new one.

1. Make the production key pair one time, outside the repository:

   ```powershell
   cargo run --example keygen -- C:\secrets\nea-update.key keys\update-public.hex
   ```

   Keep the private key in a secrets manager. Never commit it. `.gitignore`
   contains `keys/*.key`. Commit the public key (`keys/update-public.hex`),
   because the build puts it into the program. The key in the repository is
   the production key. Its private half is the secret `UPDATE_SIGNING_KEY`
   of this repository.
2. The updater reads `latest.json` from the latest release of
   `github.com/Agustin-Delgado/nea` (`RELEASES_OWNER` in `src/updater.rs`).
   The release workflow makes and uploads that file.

To sign manually, use `cargo run --example sign -- <package> <private key> [--version x.y.z] [--url url] [--out latest.json] [--check-key]`.
`--check-key` stops with an error if the key is not the key in
`keys/update-public.hex`.

### Cut a release

1. Change `version` in `Cargo.toml` (for example `0.2.0`). Run `cargo build`
   to update `Cargo.lock`. Commit both files to `main`.
2. Tag the commit and push the tag:

   ```powershell
   git tag v0.2.0
   git push origin v0.2.0
   ```

3. `.github/workflows/release.yml` starts. It does these steps:
   - It stops if the tag is not `v` + the version in `Cargo.toml`.
   - It runs `build.ps1` with the toolchain of `rust-toolchain.toml`.
   - It signs `nea.exe` for the updater with the secret `UPDATE_SIGNING_KEY`
     and `-RequireEmbeddedKey`. The key is in a temporary file only during
     this step.
   - It signs the code only if the secrets `CODE_SIGNING_PFX` (base64) and
     `CODE_SIGNING_PASSWORD` exist. Without them, SmartScreen shows a warning.
   - It creates the GitHub release with `nea.exe`, `latest.json`,
     `Nea-<version>-setup.exe`, `Nea-<version>-portable.zip` and
     `SHA256SUMS.txt`.

   `latest.json` gives the absolute URL of `nea.exe` in the tagged release.
   Installed copies find the new version in at most one day.

To test the workflow without a release, run it manually (*Actions* →
*Release* → *Run workflow*) with `dry_run` on. The run builds and signs, and
keeps the files as a workflow artifact. It does not create a release.
