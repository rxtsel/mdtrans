# mdtrans

A minimal Rust CLI for translating Markdown with AI. Streams translated Markdown to stdout by default; `preview` opens the complete translation in `$EDITOR` using a temporary file. Never modifies the input file.

> [!WARNING]
> **Work in progress.** mdtrans is under active development. Commands and configuration may change without notice, and breaking changes should be expected before v1.0.

## Installation

Requires stable Rust with support for the 2024 edition.

```bash
cargo build --release
# Binary: ./target/release/mdtrans
cargo install --path . --locked
```

Release builds optimize for size, use thin LTO and a single codegen unit, and strip symbols. This favors smaller binaries over peak runtime performance and build speed. CLI help does not include clap's optional color support. Runtime errors use red text on terminals, respecting `NO_COLOR`; pipes and files receive plain diagnostics on stderr.

## Configuration

For Gemini, start with `mdtrans login gemini`: it prompts for a hidden API key, fetches models, and lets you choose one. You can also create `mdtrans/config.toml` manually in your operating system's configuration directory:

- Linux: `${XDG_CONFIG_HOME:-$HOME/.config}/mdtrans/config.toml`
- macOS: `~/Library/Application Support/mdtrans/config.toml`
- Windows: `%APPDATA%\mdtrans\config.toml`

Configuration is required. `--lang` overrides `default_language` for the current invocation only.

### OpenAI-compatible

```toml
default_language = "es"

[provider]
type = "openai-compatible"
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"
```

```bash
export OPENAI_API_KEY='your-key'
```

Change `base_url` and `model` to use another Chat Completions-compatible service. The URL must include the API prefix (such as `/v1`), not `/chat/completions`; credentials, query parameters, and fragments are not allowed. Use HTTPS for remote services; HTTP is available for local servers. Streaming requests use `stream: true` and require `text/event-stream`, a `finish_reason = "stop"` event, and the final `[DONE]` marker. Services without SSE support can use `--no-stream`; complete responses must also report `finish_reason = "stop"`.

### Native Gemini

Replace the `[provider]` section with:

```toml
[provider]
type = "gemini"
model = "gemini-flash-latest"
```

```bash
export GEMINI_API_KEY='your-key'
```

The model is an ID without the `models/` prefix. `gemini-flash-latest` is a moving alias: its underlying version may change. Use login to discover models advertised by the API. Streaming uses `streamGenerateContent?alt=sse`; `--no-stream` and `preview` use `generateContent`. Thought/reasoning parts are not emitted; successful completion requires `finishReason = "STOP"`.

### Login and persistent credentials

```bash
mdtrans login                    # Choose a provider interactively
mdtrans login gemini             # Hidden API key → fetch models → choose a model
mdtrans login openai-compatible  # Save a key; configure base_url/model manually
```

Gemini login calls `GET /v1beta/models` with your key, follows pagination, and lists models that advertise `generateContent`. After you choose a model, it saves the key and configures Gemini to use that model. It preserves `default_language`, defaulting to `es` if no configuration exists. It rewrites the TOML (without preserving comments) and replaces the previous provider section. Invalid configuration is not overwritten. If discovery fails or you cancel before saving, credentials and configuration remain unchanged. If only the final TOML write fails, the error indicates that the key has already been saved.

The listing **does not guarantee quota, effective generation permissions, or suitability for Markdown**: some specialized models advertise the same method. Choose a general-purpose/text model. Login does not perform a test translation or switch models automatically. OpenAI-compatible login does not discover models or validate the key remotely.

Keys are stored **unencrypted** in `auth.json`, alongside `config.toml`, never inside the TOML. Writes use atomic replacement; on Unix the file has `0600` permissions. On Windows, protection depends on your user directory's permissions/ACLs. Do not share or commit this file; revoke exposed keys with your provider. To remove local credentials, delete `auth.json` (this does not revoke the keys).

Resolution order: **environment variable → auth.json**. An explicitly set but empty/invalid variable causes an error rather than falling back. To use the saved key:

```bash
unset GEMINI_API_KEY
mdtrans README.md
```

Login requires an interactive terminal; use environment variables for automation. API keys are not accepted as CLI arguments.

### Provider errors

- **401**: credential rejected.
- **403**: permissions/project access issue.
- **404**: model or endpoint not found/available for this API. Check `model`; for Gemini, run `mdtrans login gemini` to choose again. This does not by itself prove that the model does not exist globally.
- **429**: rate limit or quota exceeded; check billing/quota instead of blindly switching models.
- **503**: service temporarily unavailable or overloaded; try again later. Requests are not retried automatically.

Errors appear in red on terminal stderr, never inside the Markdown stream. Set `NO_COLOR` to disable color; redirected stderr and `TERM=dumb` also disable it. Remote error bodies are not printed because they might contain keys or document contents.

## Usage

```bash
mdtrans README.md                         # Stream Markdown to stdout
mdtrans README.md --lang fr               # Stream a French translation
mdtrans README.md --stdout                # Explicit stdout; same as the default
mdtrans README.md --no-stream             # Print only after successful completion
mdtrans README.md --lang fr --no-stream > README.fr.md
mdtrans preview README.md                # Open README-es-translate.md in $EDITOR
mdtrans preview README.md --lang fr
mdtrans --help
mdtrans --version
```

`--stdout` is retained for compatibility but is no longer necessary. Never redirect to the input file (`mdtrans README.md > README.md`): the shell would truncate it before the program runs. For filenames matching a subcommand (`preview`, `login`, `help`), use `./name`; for names starting with `-`, use `--`.

### Streaming and failures

Text is flushed to stdout as the provider sends it, including when stdout is piped. This is real provider streaming, not a typing animation. It improves feedback but does not prevent HTTP 503 errors or eliminate the wait for the first fragment. The spinner stops before the first text is printed.

**A stream can fail after partial Markdown has already been written.** On quota errors, truncated generation, malformed events, or a connection ending before the completion marker, the command exits unsuccessfully and warns on stderr that the output is incomplete. Already written text is not removed, rewritten, or mixed with diagnostics. Check the exit status before treating streamed output as a complete translation. No automatic retry or fallback request is made. Closing a pipe early (for example with `head`) stops output without reporting a provider failure.

Use `--no-stream` to withhold all Markdown until the provider response has been validated. On provider failure, it writes nothing to stdout. This does **not** make shell redirection atomic: the shell creates/truncates the destination before the command runs, and local write failures may still leave partial data.

### Editor preview

```bash
export EDITOR='nvim'
# For graphical editors, use their wait option:
export EDITOR='code --wait'
mdtrans preview README.md
```

`$EDITOR` supports arguments and quoting, but is not executed through a shell: variables, `~`, pipes, and substitutions are not expanded. It is required only for `preview`. Preview always waits for a complete, validated response before launching the editor; there is no live file rewriting or editor-specific RPC.

The file is named `<name>-<language>-translate.md` inside a unique temporary directory, such as `/tmp/mdtrans-XXXX/README-fr-translate.md`. It uses the system temporary directory (respecting its configuration, such as `TMPDIR` on Unix), not a `tmp/` folder in the project. Language characters that are unsafe in filenames are replaced with `_`.

The directory exists until the editor process exits and is then deleted, including when the editor fails. Changes made there are not saved to the original: use “Save As” **outside the temporary directory** to keep them. A graphical editor that detaches needs its wait option, such as `code --wait`. Abrupt program termination can leave temporary files behind: cleanup on reboot depends on the operating system and is not guaranteed.

While waiting for the first text, a dots spinner with elapsed time appears on **stderr**. For `--no-stream`, `preview`, and model discovery it stays until the request completes. It indicates waiting, not a percentage. It clears before streaming text, opening the editor, or displaying errors. It is disabled when stderr is not a terminal or `TERM=dumb`.

Errors go to stderr and return a nonzero exit code. Translation output has no banners or added newlines. Preview lets the editor inherit the terminal.

## Neovim integration

Use stdout with an asynchronous keymap to stream into a vertical split in your existing Neovim instance. The [Neovim guide](docs/neovim.md) includes a keymap with persistent progress and red inline errors that do not become part of the Markdown. Calling `preview` inside Neovim would launch another `$EDITOR` process instead.

## Scope and architecture

The entire UTF-8 document is sent to the selected provider; this may incur costs and expose its contents to that service. The shared prompt asks the model to preserve structure, URLs, code, frontmatter, embedded HTML, Mermaid, and LaTeX. Preservation depends on the model: **there is no byte-for-byte guarantee or AST validation**. Review important translations. Code fences are not stripped automatically because they may belong to the original document. Empty, blocked, or incomplete responses are reported as errors; streaming may have already emitted partial text. Requests have a 120-second total timeout, including streamed responses. There are no retries or input chunking for documents exceeding the model's context window. SSE parsing handles split UTF-8 characters, line endings, and event boundaries without buffering the whole output; individual SSE events are limited to 1 MiB.

Modules are organized by feature: `translate`, `preview`, `config`, and `auth`; `providers` contains HTTP adapters. `main.rs` wires the CLI, configuration, and a `Box<dyn Translator>`. The only trait represents the translation boundary; new providers do not require changes to the use case. KISS, YAGNI, OCP, and lightweight Ports & Adapters without ceremonial layers. Agent rules are in [AGENTS.md](AGENTS.md).

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```

Tests use a fake Translator, sample responses, and local HTTP; they do not call real APIs.
