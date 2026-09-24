use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc,
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
    mock_server(move |stream| {
        write!(stream, "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    })
}

fn mock_server(
    reply: impl FnOnce(&mut TcpStream) + Send + 'static,
) -> (String, thread::JoinHandle<(String, Value)>) {
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
        reply(&mut stream);
        (headers, value)
    });
    (url, handle)
}

fn sse_headers(stream: &mut TcpStream) {
    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nConnection: close\r\n\r\n").unwrap();
}

fn delta(text: &str, finish: Option<&str>) -> String {
    format!(
        "data: {}\n\n",
        json!({"choices": [{"index": 0, "delta": {"content": text}, "finish_reason": finish}]})
    )
}

#[test]
fn default_and_stdout_stream_before_the_response_completes() {
    for args in [vec![], vec!["--stdout"]] {
        let fixture = Fixture::new();
        let original = std::fs::read(&fixture.source).unwrap();
        let (release, wait) = mpsc::channel();
        let (url, server) = mock_server(move |stream| {
            sse_headers(stream);
            stream.write_all(delta("# Hola", None).as_bytes()).unwrap();
            stream.flush().unwrap();
            // The client must publish the first fragment before the server is
            // allowed to send the remaining text or close the response.
            wait.recv_timeout(Duration::from_secs(8)).unwrap();
            stream
                .write_all(delta(" 世界\n", Some("stop")).as_bytes())
                .unwrap();
            stream.write_all(b"data: [DONE]\n\n").unwrap();
        });
        fixture.configure(&url);
        let mut child = fixture
            .command()
            .arg(&fixture.source)
            .args(args)
            .env("OPENAI_API_KEY", "fake-key")
            .env("EDITOR", "must-not-launch")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (received, receiving) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut first = [0; 6];
            let result = stdout.read_exact(&mut first);
            received.send(result.map(|()| first)).unwrap();
            let mut rest = Vec::new();
            stdout.read_to_end(&mut rest).unwrap();
            rest
        });
        let early = receiving.recv_timeout(Duration::from_secs(5));
        // Always release the server, including on a failed assertion below.
        release.send(()).unwrap();
        let result = child.wait_with_output().unwrap();
        let rest = reader.join().unwrap();
        assert_eq!(early.unwrap().unwrap(), *b"# Hola");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(rest, " 世界\n".as_bytes());
        assert!(result.stderr.is_empty());
        assert_eq!(std::fs::read(&fixture.source).unwrap(), original);
        let (headers, body) = server.join().unwrap();
        assert_eq!(body["stream"], true);
        assert!(headers.to_lowercase().contains("accept: text/event-stream"));
    }
}

#[test]
fn interrupted_stream_preserves_partial_markdown_and_reports_failure_only_on_stderr() {
    for (tail, expected) in [
        (String::new(), "stream ended before successful completion"),
        (delta("", Some("stop")), "stream ended before successful completion"),
        ("data: [DONE]\n\n".into(), "finish_reason=stop"),
        (delta("do not emit", Some("length")), "incomplete"),
        ("data: not JSON\n\n".into(), "invalid JSON"),
        ("data: {\"error\":{\"code\":\"insufficient_quota\",\"message\":\"sensitive-provider-body fake-key\"}}\n\n".into(), "Rate limit or quota exceeded"),
    ] {
        let fixture = Fixture::new();
        let (url, server) = mock_server(move |stream| {
            sse_headers(stream);
            stream.write_all(delta("# Partial", None).as_bytes()).unwrap();
            stream.write_all(tail.as_bytes()).unwrap();
        });
        fixture.configure(&url);
        let output = fixture.command().arg(&fixture.source).env("OPENAI_API_KEY", "fake-key").output().unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stdout, b"# Partial");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("translation interrupted"), "{stderr}");
        assert!(stderr.contains("incomplete") && stderr.contains(expected), "{stderr}");
        assert!(!stderr.contains("fake-key") && !stderr.contains("sensitive-provider-body"));
        assert!(!stderr.contains("\x1b["));
        server.join().unwrap();
    }
}

#[test]
fn default_stream_rejects_http_failure_and_non_streaming_responses() {
    for (status, expected) in [
        (503, "Provider temporarily unavailable"),
        (200, "expected text/event-stream"),
    ] {
        let fixture = Fixture::new();
        let (url, server) = server(status, "sensitive-provider-body".into());
        fixture.configure(&url);
        let output = fixture
            .command()
            .arg(&fixture.source)
            .env("OPENAI_API_KEY", "fake-key")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{stderr}");
        assert!(
            !stderr.contains("partial Markdown") && !stderr.contains("sensitive-provider-body")
        );
        server.join().unwrap();
    }
}

#[cfg(unix)]
#[test]
fn closed_stdout_is_not_reported_as_a_provider_failure() {
    let fixture = Fixture::new();
    let (url, server) = mock_server(|stream| {
        sse_headers(stream);
        // Ignore disconnects: the client may stop reading as soon as stdout fails.
        let _ = stream.write_all(delta("# Hola", Some("stop")).as_bytes());
        let _ = stream.write_all(b"data: [DONE]\n\n");
    });
    fixture.configure(&url);
    let mut child = fixture
        .command()
        .arg(&fixture.source)
        .env("OPENAI_API_KEY", "fake-key")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    assert!(result.stderr.is_empty());
    server.join().unwrap();
}

#[test]
fn no_stream_translates_to_clean_stdout_with_default_and_override() {
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
            .arg("--no-stream")
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
        assert!(body.get("stream").is_none());
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
            .arg("--no-stream")
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
        .arg("--no-stream")
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
    assert!(String::from_utf8_lossy(&output.stderr).contains("OPENAI_API_KEY"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("$EDITOR"));
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
fn preview_does_not_launch_editor_for_incomplete_translation() {
    let fixture = Fixture::new();
    let marker = fixture.root.join("editor-ran");
    let editor = shell_words::join(["touch", marker.to_str().unwrap()]);
    let (url, server) = server(
        200,
        json!({"choices": [{"message": {"content": "partial"}, "finish_reason": "length"}]})
            .to_string(),
    );
    fixture.configure(&url);
    let output = fixture
        .command()
        .arg("preview")
        .arg(&fixture.source)
        .env("EDITOR", editor)
        .env("OPENAI_API_KEY", "fake-key")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!marker.exists());
    server.join().unwrap();
}

#[cfg(unix)]
#[test]
fn preview_opens_named_complete_translations_and_cleans_up() {
    for (args, language) in [
        (vec!["preview"], "es"),
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
        assert!(body.get("stream").is_none());
        assert!(
            body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .ends_with(language)
        );
    }
}
