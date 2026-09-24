# mdtrans — agent rules

- Write code, comments, diagnostics, and documentation in English. Keep multilingual test data when it verifies translation behavior.
- Idiomatic Rust: clear ownership, enums for variants, `Result` and `?` for errors. No `unwrap`/`expect` in production.
- KISS and YAGNI first. Apply SOLID without ceremony or Java-style patterns.
- Screaming Architecture and vertical slicing: organize by feature, not technical layer. Do not fragment small modules.
- Lightweight Ports & Adapters: `translate::Translator` is the boundary. The feature must not know about HTTP, reqwest, or concrete providers.
- OCP matters: new providers implement the port; only configuration and the composition root register the variant. Do not change the use case.
- Apply DIP at real boundaries. No speculative traits, factories, services, or repositories; a few duplicated lines are preferable.
- The prompt and Markdown policy belong to `translate`; adapters transport requests and validate responses.
- Never write to the input file. Stream Markdown to stdout by default; `--no-stream` waits for a complete response. `preview` opens a temporary `<name>-<language>-translate.md` in `$EDITOR`. Diagnostics go to stderr. Never log keys or documents.
- A failed stream may leave partial output: report failure, never claim completion or silently retry. Keep terminal colors out of stdout and redirected stderr; respect `NO_COLOR`.
- Credentials: environment > `auth.json`, never TOML or CLI arguments. Login hides input; writes are atomic with `0600` permissions on Unix. Do not claim encryption.
- Gemini discovers models through the API; do not maintain static lists or confuse availability with permissions/quota. Preview keeps the temporary file until `$EDITOR` exits.
- Native Rust tests where they matter: behavior, a fake Translator, and local HTTP. No real APIs or tests written just for coverage.
- Keep dependencies small. Streaming stays behind `Translator`; SSE and provider completion markers belong to adapters. Do not add caching, complex ASTs, TUIs, or speculative infrastructure.
- Before finishing: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`, `cargo build --release`. Review the entire diff.
