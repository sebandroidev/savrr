# Contributing to Savrr

Thanks for taking a look. Savrr is early, which means there's a lot to do and the code still moves around. Bug reports, save-path fixes, and small focused pull requests are all welcome.

## Getting set up

You need Rust (stable), Node 22 or newer, and pnpm. On Linux you also need the GTK and WebKit development packages for the desktop app; the exact list is in `.github/workflows/ci.yml` under the Linux deps step.

```bash
git clone https://github.com/sebandroidev/savrr
cd savrr
cargo build --workspace
cargo test --workspace
```

The desktop app has a frontend that has to be built before its Rust side will compile:

```bash
pnpm --dir crates/savr-app/ui install
pnpm --dir crates/savr-app/ui build
```

## Where things live

- `crates/savr-core` is the shared library. Types, the Ludusavi manifest parser, blake3 snapshots and diffing, the `.savr` archive format, and the REST/WebSocket/IPC contracts all live here. Change a wire type once and the compiler tells every other crate what broke. This crate has no network or filesystem side effects beyond archive read/write, so it's the easiest place to add a well-tested unit of logic.
- `crates/savr-server` is the Axum service. Runtime `sqlx` queries against SQLite, a content-addressed blob store, and the compare-and-swap that advances a game's head.
- `crates/savr-daemon` is the headless service that does detection and sync.
- `crates/savr-app` is the Tauri desktop app. Rust commands under `src-tauri`, Svelte UI under `ui`.
- `docs/prd` holds the requirement docs. If you're unsure why something works the way it does, the answer is usually in there.

## Before you open a pull request

Run the same checks CI runs:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

A few things that will make review faster:

- Keep the change focused on one thing. A small PR that does one job gets merged; a large one that does five gets stuck.
- If you change behavior, add or update a test that would fail without your change. The sync and conflict paths guard people's save files, so they need to stay covered.
- Match the style of the code around you. There's no separate style guide beyond `rustfmt` and what the neighbors do.
- Explain the why in the PR description, not just the what. The diff already shows the what.

## Adding or fixing save locations

Most "my game didn't get detected" problems are really "the manifest doesn't know where this game saves." The right fix is usually to contribute the path back to the [Ludusavi manifest](https://github.com/mtkennerly/ludusavi-manifest), which Savrr and other tools both pull from. If it's a Savrr-specific parsing bug, open an issue here with the game and the path it actually uses.

## Reporting bugs

Open an issue with your OS, which store the game came from, and what you expected versus what happened. If it's about a save being missed or restored wrong, that's high priority, say so.

## Security

Don't file security problems as public issues. See [SECURITY.md](SECURITY.md).
