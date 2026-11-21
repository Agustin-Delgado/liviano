# Contributing to Nea

Thank you for your help. Nea is small on purpose. Before you start a large
new feature, open an issue to discuss it. Then nobody does work that the
project cannot accept.

## Build

You need these items:

- Windows 10 or 11.
- Stable Rust with the `x86_64-pc-windows-msvc` target.
- The Visual Studio build tools (MSVC) with the Windows SDK (for `rc.exe`).
- The WebView2 runtime.

```powershell
cargo build            # development build: "Nea (development)"
cargo build --release  # the published version
```

Before you open a PR, run the same checks as CI:

```powershell
cargo fmt --check
cargo clippy -- -D warnings
cargo build --release
```

[docs/building.md](docs/building.md) has more details: script control,
diagnostics and packaging.

## Style

- **Write everything in English:** code, comments, interface text (menus,
  Settings, notices), documentation and commit messages.
- A comment tells why the code is as it is. It does not repeat the line.
- Use `cargo fmt` with the `rustfmt.toml` of the repository. Clippy must show
  no warnings. If you need an `allow`, give its `reason`.
- Memory is the priority. A change that uses memory at rest needs a
  measurement ([docs/benchmarks.md](docs/benchmarks.md)).
- Nea sends nothing to the network without a request from the person. Nea has
  no telemetry.

## Tests

The tests are not in this repository. They are in a private repository of
the maintainer, with the code. Thus the maintainer validates each PR before
the merge. CI here checks only the build, the format and clippy. If your
change needs a test, tell in the PR how you tested it manually.

## Pull requests

- Make one change in each PR. Several small PRs are better than one large PR.
- Tell what changes for the users of Nea and how you tested it.
- If the change affects the interface, add a screenshot in light and in dark.
- Do not report security problems in an issue or a PR. Read
  [SECURITY.md](SECURITY.md).

When you contribute, you agree to publish your work under the
[MIT license](LICENSE). You also agree to follow the
[code of conduct](CODE_OF_CONDUCT.md).
