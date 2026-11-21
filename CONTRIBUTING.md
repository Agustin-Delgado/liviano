# Contributing to Nea

Thank you for your help. Nea is small on purpose. Before you start a large
new feature, open an issue to discuss it. Then nobody does work that the
project cannot accept.

## Build

You need these items:

- Windows 10 or 11.
- [rustup](https://rustup.rs). `rust-toolchain.toml` selects the Rust version.
  CI uses the same version.
- The Visual Studio build tools (MSVC) with the Windows SDK (for `rc.exe`).
- The WebView2 runtime.

```powershell
git clone https://github.com/Agustin-Delgado/nea.git
cd nea
cargo build            # development build: "Nea (development)"
cargo build --release  # the published version
```

Before you open a PR, run the same checks as CI:

```powershell
cargo fmt --all --check
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

The tests are in a private repository. Pull requests are tested by CI. CI
mounts the tests as the submodule `tests/` and runs them for each pull
request from a branch of this repository. A pull request from a fork has no
access to the tests. For a fork, CI checks the format, clippy and the build,
and the maintainer runs the tests before the merge. If your change needs a
test, tell in the PR how you tested it manually.

If you have access to the tests, clone with `--recurse-submodules`:

```powershell
git clone --recurse-submodules https://github.com/Agustin-Delgado/nea.git
cd nea
cargo test
cargo clippy --all-targets -- -D warnings
```

Without the submodule, `cargo test` builds and runs 0 tests.
[docs/building.md](docs/building.md#tests) tells how the tests connect to
the code.

## Pull requests

- Make one change in each PR. Several small PRs are better than one large PR.
- Tell what changes for the users of Nea and how you tested it.
- If the change affects the interface, add a screenshot in light and in dark.
- Do not report security problems in an issue or a PR. Read
  [SECURITY.md](SECURITY.md).

When you contribute, you agree to publish your work under the
[MIT license](LICENSE). You also agree to follow the
[code of conduct](CODE_OF_CONDUCT.md).
