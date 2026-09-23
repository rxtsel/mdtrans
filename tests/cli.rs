use std::{
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
    source: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_owned();
        let config = if cfg!(target_os = "macos") {
            root.join("Library/Application Support/mdtrans/config.toml")
        } else {
            root.join("mdtrans/config.toml")
        };
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let source = root.join("README.md");
        std::fs::write(&source, "# Hello\n\n```sh\necho hello\n```\n").unwrap();
        Self {
            _directory: directory,
            root,
            config,
            source,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mdtrans"));
        command
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", &self.root)
            .env("APPDATA", &self.root)
            .env("USERPROFILE", &self.root)
            .env_remove("OPENAI_API_KEY")
            .env_remove("GEMINI_API_KEY")
            .env_remove("EDITOR")
            .env("NO_PROXY", "*")
            .env("no_proxy", "*");
        command
    }

    fn configure(&self, url: &str) {
        std::fs::write(&self.config, format!("default_language = 'es'\n[provider]\ntype = 'openai-compatible'\nbase_url = '{url}/v1'\nmodel = 'local-test'\n")).unwrap();
    }
}

// One real HTTP exchange, using only std and a loopback listener. Bounded waits
// ensure a broken CLI fails the test instead of hanging the suite indefinitely.
fn server(status: u16, body: String) -> (String, thread::JoinHandle<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(e) => panic!("mock server accept failed: {e}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let (headers, value) = loop {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0, "unexpected EOF");
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break (
                        headers,
                        serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + length]).unwrap(),
                    );
                }
            }
        };
        write!(stream, "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        (headers, value)
    });
    (url, handle)
}

#[test]
fn translates_to_clean_stdout_with_default_and_override() {
    let fixture = Fixture::new();
    let original = std::fs::read(&fixture.source).unwrap();
    for (args, language) in [(vec![], "es"), (vec!["--lang", "fr"], "fr")] {
        let translated = "# Hola\n\n```sh\necho hello\n```\n";
        let (url, server) = server(
            200,
            json!({"choices": [{"message": {"content": translated}, "finish_reason": "stop"}]})
                .to_string(),
        );
        fixture.configure(&url);
        let output = fixture
            .command()
            .arg(&fixture.source)
            .arg("--stdout")
            .args(args)
            .env("OPENAI_API_KEY", "fake-key")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, translated.as_bytes());
        assert!(output.stderr.is_empty());
        assert_eq!(std::fs::read(&fixture.source).unwrap(), original);
        let (headers, body) = server.join().unwrap();
        assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1"));
        assert!(
            headers
                .to_lowercase()
                .contains("authorization: bearer fake-key")
        );
        assert_eq!(body["model"], "local-test");
        assert_eq!(
            body["messages"][1]["content"],
            String::from_utf8(original.clone()).unwrap()
        );
        assert!(
            body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .ends_with(language)
        );
    }
}

#[test]
fn provider_errors_never_write_stdout_or_echo_response_bodies() {
    let fixture = Fixture::new();
    for (status, body, expected) in [
        (401, "sensitive-provider-body", "API key rejected"),
        (403, "sensitive-provider-body", "Access denied"),
        (
            404,
            "sensitive-provider-body",
            "Model or API endpoint not found/available",
        ),
        (
            429,
            "sensitive-provider-body",
            "Rate limit or quota exceeded",
        ),
        (
            503,
            "sensitive-provider-body",
            "Provider temporarily unavailable or overloaded",
        ),
        (200, "not json", "invalid provider response"),
        (
            200,
            r#"{"choices":[{"message":{"content":"partial"},"finish_reason":"length"}]}"#,
            "incomplete",
        ),
    ] {
        let (url, server) = server(status, body.into());
        fixture.configure(&url);
        let output = fixture
            .command()
            .arg(&fixture.source)
            .arg("--stdout")
            .env("OPENAI_API_KEY", "fake-key")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{stderr}");
        assert!(!stderr.contains("sensitive-provider-body"));
        assert!(!stderr.contains("fake-key"));
        assert!(
            !stderr.contains("\x1b["),
            "no terminal animation in redirected stderr"
        );
        assert!(!stderr.contains("waiting for provider"));
        server.join().unwrap();
    }
}

#[test]
fn login_requires_terminal_and_does_not_create_credentials_on_failure() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args(["login", "gemini"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
    assert!(!fixture.config.with_file_name("auth.json").exists());
}

#[test]
fn saved_key_is_used_when_environment_is_absent() {
    let fixture = Fixture::new();
    let auth = fixture.config.with_file_name("auth.json");
    // Match the permissions used by login, without putting a key in process args.
    let mut file = tempfile::NamedTempFile::new_in(auth.parent().unwrap()).unwrap();
    file.write_all(br#"{"OPENAI_API_KEY":"saved-fake-key"}"#)
        .unwrap();
    file.persist(&auth).unwrap();
    let (url, server) = server(
        200,
        json!({"choices": [{"message": {"content": "# Hola"}, "finish_reason": "stop"}]})
            .to_string(),
    );
    fixture.configure(&url);
    let output = fixture
        .command()
        .arg(&fixture.source)
        .arg("--stdout")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"# Hola");
    let (headers, _) = server.join().unwrap();
    assert!(
        headers
            .to_lowercase()
            .contains("authorization: bearer saved-fake-key")
    );
}

#[test]
fn help_version_and_actionable_local_errors() {
    let fixture = Fixture::new();
    for flag in ["--help", "--version"] {
        let output = fixture.command().arg(flag).output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
    }
    let output = fixture
        .command()
        .arg(&fixture.source)
        .arg("--stdout")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("configuration"));
    fixture.configure("http://127.0.0.1:1");
    let output = fixture
        .command()
        .arg(&fixture.source)
        .arg("--stdout")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("OPENAI_API_KEY"));
    let output = fixture.command().arg(&fixture.source).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("$EDITOR"));
    let output = fixture
        .command()
        .arg("preview")
        .arg(&fixture.source)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("$EDITOR"));
    let output = fixture
        .command()
        .arg(fixture.root.join("missing.md"))
        .arg("--stdout")
        .env("OPENAI_API_KEY", "fake-key")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot read Markdown file"));
}

#[test]
fn selects_gemini_credentials_and_adapter_without_network() {
    let fixture = Fixture::new();
    std::fs::write(
        &fixture.config,
        "default_language = 'es'\n[provider]\ntype = 'gemini'\nmodel = 'gemini-2.5-flash'\n",
    )
    .unwrap();
    let output = fixture
        .command()
        .arg(&fixture.source)
        .arg("--stdout")
        .env("OPENAI_API_KEY", "wrong-provider-key")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("GEMINI_API_KEY"));
    std::fs::write(&fixture.source, "").unwrap();
    let output = fixture
        .command()
        .arg(&fixture.source)
        .arg("--stdout")
        .env("GEMINI_API_KEY", "fake-key")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn default_and_preview_open_named_translations_and_clean_up() {
    for (args, language) in [
        (vec![], "es"),
        (vec!["--lang", "fr"], "fr"),
        (vec!["preview", "--lang", "fr"], "fr"),
    ] {
        let fixture = Fixture::new();
        let original = std::fs::read(&fixture.source).unwrap();
        let script = fixture.root.join("fake editor.sh");
        let snapshot = fixture.root.join("saved.md");
        let recorded = fixture.root.join("temporary-path");
        std::fs::write(&script, "cp \"$3\" \"$1\" && printf '%s' \"$3\" > \"$2\"\n").unwrap();
        let editor = shell_words::join([
            "sh",
            script.to_str().unwrap(),
            snapshot.to_str().unwrap(),
            recorded.to_str().unwrap(),
        ]);
        let (url, server) = server(
            200,
            json!({"choices": [{"message": {"content": "# Bonjour\n"}, "finish_reason": "stop"}]})
                .to_string(),
        );
        fixture.configure(&url);
        let output = fixture
            .command()
            .args(args)
            .arg(&fixture.source)
            .env("EDITOR", editor)
            .env("OPENAI_API_KEY", "fake-key")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert_eq!(std::fs::read_to_string(snapshot).unwrap(), "# Bonjour\n");
        let temporary = PathBuf::from(std::fs::read_to_string(recorded).unwrap());
        assert_eq!(
            temporary.file_name().unwrap(),
            format!("README-{language}-translate.md").as_str()
        );
        assert!(!temporary.exists());
        assert!(!temporary.parent().unwrap().exists());
        assert!(
            !fixture
                .root
                .join(format!("README-{language}-translate.md"))
                .exists()
        );
        assert_eq!(std::fs::read(&fixture.source).unwrap(), original);
        let (_, body) = server.join().unwrap();
        assert!(
            body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .ends_with(language)
        );
    }
}
