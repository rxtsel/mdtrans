use std::{future::Future, pin::Pin};

use anyhow::{Context, Result, bail};
use reqwest::{Client, Url, header::HeaderValue};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::translate::{TranslationError, TranslationRequest, Translator};

pub struct Gemini {
    client: Client,
    endpoint: Url,
    key: HeaderValue,
}

impl Gemini {
    pub fn new(client: Client, model: &str, key: &str) -> Result<Self> {
        // Model is a model ID, not a URL or path (e.g. gemini-2.5-flash).
        if !valid_model_id(model) {
            bail!("Gemini model must be a model ID, without a models/ prefix");
        }
        let endpoint = Url::parse(&format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
        ))?;
        let mut key = HeaderValue::from_str(key).context("invalid GEMINI_API_KEY header value")?;
        key.set_sensitive(true);
        Ok(Self {
            client,
            endpoint,
            key,
        })
    }

    fn body(request: &TranslationRequest<'_>) -> Value {
        json!({
            "systemInstruction": {"parts": [{"text": request.system_prompt()}]},
            "contents": [{"role": "user", "parts": [{"text": request.content}]}]
        })
    }
}

impl Translator for Gemini {
    fn translate<'a>(
        &'a self,
        request: TranslationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<String, TranslationError>> + Send + 'a>> {
        Box::pin(async move {
            let response: Response = super::send_json(
                self.client
                    .post(self.endpoint.clone())
                    .header("x-goog-api-key", self.key.clone())
                    .json(&Self::body(&request)),
            )
            .await?;
            response.markdown()
        })
    }
}

fn valid_model_id(model: &str) -> bool {
    !model.is_empty()
        && model
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

pub async fn list_models(client: &Client, key: &str) -> Result<Vec<String>> {
    list_models_at(
        client,
        key,
        Url::parse("https://generativelanguage.googleapis.com/v1beta/models")?,
    )
    .await
}

async fn list_models_at(client: &Client, key: &str, endpoint: Url) -> Result<Vec<String>> {
    let mut key = HeaderValue::from_str(key).context("invalid GEMINI_API_KEY header value")?;
    key.set_sensitive(true);
    let mut models = std::collections::BTreeSet::new();
    let mut seen_tokens = std::collections::HashSet::new();
    let mut token = String::new();
    loop {
        let mut request = client
            .get(endpoint.clone())
            .header("x-goog-api-key", key.clone())
            .query(&[("pageSize", "1000")]);
        if !token.is_empty() {
            request = request.query(&[("pageToken", &token)]);
        }
        let page: ModelsPage = super::send_json(request)
            .await
            .context("cannot list Gemini models")?;
        models.extend(
            page.models
                .into_iter()
                .filter_map(|model| model.generation_id()),
        );
        match page.next_page_token.filter(|token| !token.is_empty()) {
            Some(next) if seen_tokens.insert(next.clone()) => token = next,
            Some(_) => bail!("invalid Gemini model listing: repeated page token"),
            None => break,
        }
    }
    Ok(models.into_iter().collect())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelsPage {
    #[serde(default)]
    models: Vec<Model>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Model {
    name: String,
    #[serde(default)]
    supported_generation_methods: Vec<String>,
}

impl Model {
    fn generation_id(self) -> Option<String> {
        let id = self.name.strip_prefix("models/")?;
        (valid_model_id(id)
            && self
                .supported_generation_methods
                .iter()
                .any(|method| method == "generateContent"))
        .then(|| id.to_owned())
    }
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    candidates: Vec<Candidate>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    finish_reason: Option<String>,
    content: Option<Content>,
}
#[derive(Deserialize)]
struct Content {
    parts: Vec<Part>,
}
#[derive(Deserialize)]
struct Part {
    text: Option<String>,
    #[serde(default)]
    thought: bool,
}

impl Response {
    fn markdown(self) -> Result<String, TranslationError> {
        let candidate =
            self.candidates
                .into_iter()
                .next()
                .ok_or(TranslationError::InvalidResponse(
                    "no Gemini candidate (possibly blocked)",
                ))?;
        if candidate.finish_reason.as_deref() != Some("STOP") {
            return Err(TranslationError::InvalidResponse(
                "Gemini generation incomplete or blocked (expected finishReason=STOP)",
            ));
        }
        let content = candidate
            .content
            .ok_or(TranslationError::InvalidResponse("missing Gemini content"))?;
        let text: String = content
            .parts
            .into_iter()
            .filter(|part| !part.thought)
            .filter_map(|part| part.text)
            .collect();
        if text.trim().is_empty() {
            return Err(TranslationError::InvalidResponse(
                "missing or empty Gemini text",
            ));
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_native_endpoint_and_request() {
        let provider = Gemini::new(Client::new(), "gemini-2.5-flash", "test-key").unwrap();
        assert_eq!(
            provider.endpoint.as_str(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent"
        );
        assert!(Gemini::new(Client::new(), "models/test?key=x", "key").is_err());
        let body = Gemini::body(&TranslationRequest {
            content: "# Hello",
            target_language: "fr",
        });
        assert_eq!(body["contents"][0]["parts"][0]["text"], "# Hello");
        assert!(
            body["systemInstruction"]["parts"][0]["text"]
                .as_str()
                .unwrap()
                .ends_with("fr")
        );
    }

    fn model_server(pages: Vec<(u16, Value)>) -> (Url, std::thread::JoinHandle<Vec<String>>) {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
            time::{Duration, Instant},
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = Url::parse(&format!(
            "http://{}/v1beta/models",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in pages {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(10))
                        }
                        Err(e) => panic!("model server accept failed: {e}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    let mut buffer = [0; 1024];
                    let size = stream.read(&mut buffer).unwrap();
                    assert!(size > 0 && bytes.len() < 16384);
                    bytes.extend_from_slice(&buffer[..size]);
                }
                requests.push(String::from_utf8(bytes).unwrap());
                let body = body.to_string();
                write!(stream, "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            requests
        });
        (url, handle)
    }

    #[tokio::test]
    async fn discovers_paginated_generation_models_with_key_in_header() {
        let (url, server) = model_server(vec![
            (
                200,
                json!({"models": [
                {"name": "models/gemini-b", "supportedGenerationMethods": ["generateContent"]},
                {"name": "models/embedding", "supportedGenerationMethods": ["embedContent"]},
                {"name": "models/unknown"},
                {"name": "models/unsafe\nname", "supportedGenerationMethods": ["generateContent"]}
            ], "nextPageToken": "next+page"}),
            ),
            (
                200,
                json!({"models": [
                    {"name": "models/gemini-a", "supportedGenerationMethods": ["generateContent", "countTokens"]},
                    {"name": "models/gemini-b", "supportedGenerationMethods": ["generateContent"]}
                ]}),
            ),
        ]);
        let client = Client::builder().no_proxy().build().unwrap();
        assert_eq!(
            list_models_at(&client, "fake-secret", url).await.unwrap(),
            ["gemini-a", "gemini-b"]
        );
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("GET /v1beta/models?pageSize=1000 HTTP/1.1"));
        assert!(
            requests[1]
                .starts_with("GET /v1beta/models?pageSize=1000&pageToken=next%2Bpage HTTP/1.1")
        );
        for request in requests {
            assert!(
                request
                    .to_lowercase()
                    .contains("x-goog-api-key: fake-secret")
            );
            assert!(!request.lines().next().unwrap().contains("fake-secret"));
        }
    }

    #[tokio::test]
    async fn discovery_propagates_http_errors_without_response_secrets() {
        let (url, server) = model_server(vec![(403, json!({"error": "fake-secret"}))]);
        let client = Client::builder().no_proxy().build().unwrap();
        let error = list_models_at(&client, "fake-secret", url)
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<TranslationError>(),
            Some(TranslationError::Http(403))
        ));
        assert!(!format!("{error:#}").contains("fake-secret"));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn discovery_rejects_repeated_page_tokens() {
        let page = json!({"models": [], "nextPageToken": "same"});
        let (url, server) = model_server(vec![(200, page.clone()), (200, page)]);
        let client = Client::builder().no_proxy().build().unwrap();
        assert!(
            list_models_at(&client, "fake-secret", url)
                .await
                .unwrap_err()
                .to_string()
                .contains("repeated page token")
        );
        server.join().unwrap();
    }

    #[test]
    fn joins_text_without_exposing_thoughts() {
        let response: Response = serde_json::from_value(json!({"candidates": [{"finishReason": "STOP", "content": {"parts": [{"text": "private reasoning", "thought": true}, {"text": "# Bonjour"}, {"text": "\n"}]}}]})).unwrap();
        assert_eq!(response.markdown().unwrap(), "# Bonjour\n");
    }

    #[test]
    fn rejects_blocked_truncated_and_empty_responses() {
        for value in [
            json!({"promptFeedback": {"blockReason": "SAFETY"}}),
            json!({"candidates": [{"finishReason": "MAX_TOKENS", "content": {"parts": [{"text": "partial"}]}}]}),
            json!({"candidates": [{"finishReason": "STOP", "content": {"parts": []}}]}),
        ] {
            assert!(
                serde_json::from_value::<Response>(value)
                    .unwrap()
                    .markdown()
                    .is_err()
            );
        }
    }
}
