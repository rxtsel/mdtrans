# mdtrans

A minimal Rust CLI for translating Markdown with AI. Opens the translation in `$EDITOR` using a temporary file; with `--stdout`, prints the Markdown to the console. Never modifies the input file.

> [!WARNING]
> **Work in progress.** mdtrans is under active development. Commands and configuration may change without notice, and breaking changes should be expected before v1.0.

## Installation

Requires stable Rust with support for the 2024 edition.

```bash
cargo build --release
# Binary: ./target/release/mdtrans
cargo install --path . --locked
```

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

Change `base_url` and `model` to use another Chat Completions-compatible service. The URL must include the API prefix (such as `/v1`), not `/chat/completions`; credentials, query parameters, and fragments are not allowed. Use HTTPS for remote services; HTTP is available for local servers. The service must return `finish_reason = "stop"` when the translation is complete.

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

The model is an ID without the `models/` prefix. `gemini-flash-latest` is a moving alias: its underlying version may change. Use login to discover models advertised by the API.

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

Remote error bodies are not printed because they might contain keys or document contents.

## Usage

```bash
mdtrans README.md                         # Open README-es-translate.md in $EDITOR
mdtrans README.md --lang fr               # Open README-fr-translate.md in $EDITOR
mdtrans README.md --stdout                # Markdown only on stdout; no $EDITOR required
mdtrans README.md --lang fr --stdout > README.fr.md
mdtrans preview README.md                # Compatibility: same as the default behavior
mdtrans --help
mdtrans --version
```

Redirecting stdout does not automatically enable console output: always use `--stdout` for pipes or files. Never redirect to the input file (`mdtrans README.md --stdout > README.md`): the shell would truncate it before the program runs. For filenames matching a subcommand (`preview`, `login`, `help`), use `./name`; for names starting with `-`, use `--`.

```bash
export EDITOR='nvim'
# For graphical editors, use their wait option:
export EDITOR='code --wait'
mdtrans preview README.md
```

`$EDITOR` supports arguments and quoting, but is not executed through a shell: variables, `~`, pipes, and substitutions are not expanded. It is required unless `--stdout` is used.

The file is named `<name>-<language>-translate.md` inside a unique temporary directory, such as `/tmp/mdtrans-XXXX/README-fr-translate.md`. It uses the system temporary directory (respecting its configuration, such as `TMPDIR` on Unix), not a `tmp/` folder in the project. Language characters that are unsafe in filenames are replaced with `_`.

The directory exists until the editor process exits and is then deleted, including when the editor fails. Changes made there are not saved to the original: use “Save As” **outside the temporary directory** to keep them. A graphical editor that detaches needs its wait option, such as `code --wait`. Abrupt program termination can leave temporary files behind: cleanup on reboot depends on the operating system and is not guaranteed.

While translating or discovering models, a dots spinner with elapsed time appears on **stderr** (`⠋ Translating… [00:12]`). It does not indicate a percentage or actual model progress: it only shows that the program is still waiting. It clears on completion or failure, before opening the editor or displaying errors. It is disabled when stderr is not a terminal or `TERM=dumb`. With `--stdout`, you can redirect Markdown to a file while keeping terminal feedback.

Errors go to stderr and return a nonzero exit code. Translation output has no banners or added newlines. Preview lets the editor inherit the terminal.

## Scope and architecture

The entire UTF-8 document is sent to the selected provider; this may incur costs and expose its contents to that service. The shared prompt asks the model to preserve structure, URLs, code, frontmatter, embedded HTML, Mermaid, and LaTeX. Preservation depends on the model: **there is no byte-for-byte guarantee or AST validation**. Review important translations. Code fences are not stripped automatically because they may belong to the original document. Empty, blocked, or incomplete responses are rejected. Requests have a 120-second timeout; there are no retries or chunking for documents exceeding the model's context window.

Modules are organized by feature: `translate`, `preview`, `config`, and `auth`; `providers` contains HTTP adapters. `main.rs` wires the CLI, configuration, and a `Box<dyn Translator>`. The only trait represents the translation boundary; new providers do not require changes to the use case. KISS, YAGNI, OCP, and lightweight Ports & Adapters without ceremonial layers. Agent rules are in [AGENTS.md](AGENTS.md).

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```

Tests use a fake Translator, sample responses, and local HTTP; they do not call real APIs.
