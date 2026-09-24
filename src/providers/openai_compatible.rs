use std::{future::Future, ops::ControlFlow, pin::Pin};

use anyhow::{Context, Result, bail};
use reqwest::{
    Client, Url,
    header::{AUTHORIZATION, HeaderValue},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::translate::{TextSink, TranslationError, TranslationRequest, Translator};

pub struct OpenAiCompatible {
    client: Client,
    endpoint: Url,
    model: String,
    authorization: HeaderValue,
}

impl OpenAiCompatible {
    pub fn new(client: Client, base_url: &str, model: String, key: &str) -> Result<Self> {
        let mut endpoint = Url::parse(base_url).context("invalid provider base_url")?;
        if !matches!(endpoint.scheme(), "https" | "http")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            bail!("base_url must be an HTTP(S) URL without credentials, query or fragment");
        }
        let path = format!("{}/chat/completions", endpoint.path().trim_end_matches('/'));
        endpoint.set_path(&path);
        let mut authorization = HeaderValue::from_str(&format!("Bearer {key}"))
            .context("invalid OPENAI_API_KEY header value")?;
        authorization.set_sensitive(true);
        Ok(Self {
            client,
            endpoint,
            model,
            authorization,
        })
    }

    fn body(&self, request: &TranslationRequest<'_>) -> Value {
        json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": request.system_prompt()},
                {"role": "user", "content": request.content}
            ]
        })
    }
}

impl Translator for OpenAiCompatible {
    fn translate<'a>(
        &'a self,
        request: TranslationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<String, TranslationError>> + Send + 'a>> {
        Box::pin(async move {
            let response: Response = super::send_json(
                self.client
                    .post(self.endpoint.clone())
                    .header(AUTHORIZATION, self.authorization.clone())
                    .json(&self.body(&request)),
            )
            .await?;
            response.markdown()
        })
    }

    fn stream<'a>(
        &'a self,
        request: TranslationRequest<'a>,
        on_text: &'a mut TextSink<'_>,
    ) -> Pin<Box<dyn Future<Output = Result<(), TranslationError>> + Send + 'a>> {
        Box::pin(async move {
            let mut body = self.body(&request);
            body["stream"] = json!(true);
            let mut state = StreamState::default();
            super::sse::consume(
                self.client
                    .post(self.endpoint.clone())
                    .header(AUTHORIZATION, self.authorization.clone())
                    .json(&body),
                |data| state.handle(data, on_text),
            )
            .await
        })
    }
}

#[derive(Default)]
struct StreamState {
    stopped: bool,
    has_text: bool,
}

#[derive(Deserialize)]
struct StreamResponse {
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    index: usize,
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Delta {
    content: Option<String>,
    refusal: Option<String>,
}

impl StreamState {
    fn handle(
        &mut self,
        data: &str,
        on_text: &mut TextSink<'_>,
    ) -> Result<ControlFlow<()>, TranslationError> {
        if data.trim() == "[DONE]" {
            if !self.stopped || !self.has_text {
                return Err(TranslationError::InvalidResponse(
                    "stream completed without nonempty text and finish_reason=stop",
                ));
            }
            return Ok(ControlFlow::Break(()));
        }
        let response: StreamResponse = super::sse::json(data)?;
        // Usage-only events have no choices. Never mix different candidates.
        let Some(choice) = response
            .choices
            .into_iter()
            .find(|choice| choice.index == 0)
        else {
            return Ok(ControlFlow::Continue(()));
        };
        if choice.delta.refusal.is_some() {
            return Err(TranslationError::InvalidResponse(
                "provider refused the translation",
            ));
        }
        if choice
            .finish_reason
            .as_deref()
            .is_some_and(|reason| reason != "stop")
        {
            return Err(TranslationError::InvalidResponse(
                "generation incomplete or refused (expected finish_reason=stop)",
            ));
        }
        if let Some(text) = choice.delta.content.filter(|text| !text.is_empty()) {
            if self.stopped {
                return Err(TranslationError::InvalidResponse(
                    "text received after stream completion",
                ));
            }
            self.has_text |= !text.trim().is_empty();
            on_text(&text)?;
        }
        self.stopped |= choice.finish_reason.as_deref() == Some("stop");
        Ok(ControlFlow::Continue(()))
    }
}

#[derive(Deserialize)]
struct Response {
    choices: Vec<Choice>,
}
#[derive(Deserialize)]
struct Choice {
    message: Message,
    finish_reason: Option<String>,
}
#[derive(Deserialize)]
struct Message {
    content: Option<String>,
}

impl Response {
    fn markdown(self) -> Result<String, TranslationError> {
        let choice = self
            .choices
            .into_iter()
            .next()
            .ok_or(TranslationError::InvalidResponse("no choices"))?;
        if choice.finish_reason.as_deref() != Some("stop") {
            return Err(TranslationError::InvalidResponse(
                "generation incomplete or refused (expected finish_reason=stop)",
            ));
        }
        choice
            .message
            .content
            .filter(|text| !text.trim().is_empty())
            .ok_or(TranslationError::InvalidResponse("missing or empty text"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_configured_endpoint_and_markdown_request() {
        for base in ["http://localhost:1234/v1", "http://localhost:1234/v1/"] {
            let provider =
                OpenAiCompatible::new(Client::new(), base, "test-model".into(), "test-key")
                    .unwrap();
            assert_eq!(
                provider.endpoint.as_str(),
                "http://localhost:1234/v1/chat/completions"
            );
            let body = provider.body(&TranslationRequest {
                content: "# Hello",
                target_language: "fr",
            });
            assert_eq!(body["model"], "test-model");
            assert_eq!(body["messages"][1]["content"], "# Hello");
            assert!(
                body["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .ends_with("fr")
            );
        }
        for base in [
            "invalid",
            "file:///tmp",
            "https://user:pass@example.org",
            "https://example.org?key=secret",
        ] {
            assert!(OpenAiCompatible::new(Client::new(), base, "test".into(), "key").is_err());
        }
    }

    #[test]
    fn streams_deltas_and_requires_stop_then_done() {
        let mut state = StreamState::default();
        let mut fragments = Vec::new();
        let mut sink = |text: &str| {
            fragments.push(text.to_owned());
            Ok(())
        };
        for event in [
            json!({"choices": [{"index": 0, "delta": {"role": "assistant"}}]}),
            json!({"choices": [{"index": 0, "delta": {"content": "# Hé"}}]}),
            json!({"choices": [{"index": 0, "delta": {"content": "llo\n"}, "finish_reason": "stop"}]}),
            json!({"choices": [], "usage": {"completion_tokens": 3}}),
        ] {
            assert!(
                state
                    .handle(&event.to_string(), &mut sink)
                    .unwrap()
                    .is_continue()
            );
        }
        assert!(state.handle("[DONE]", &mut sink).unwrap().is_break());
        assert_eq!(fragments, ["# Hé", "llo\n"]);
        assert!(
            StreamState::default()
                .handle("[DONE]", &mut |_| Ok(()))
                .is_err()
        );
    }

    #[test]
    fn rejects_failed_streams_without_emitting_the_failed_chunk() {
        for event in [
            json!({"choices": [{"delta": {"content": "partial"}, "finish_reason": "length"}]}),
            json!({"choices": [{"delta": {"refusal": "refused"}, "finish_reason": "stop"}]}),
            json!({"error": {"code": "insufficient_quota"}}),
        ] {
            let mut fragments = Vec::new();
            assert!(
                StreamState::default()
                    .handle(&event.to_string(), &mut |text| {
                        fragments.push(text.to_owned());
                        Ok(())
                    })
                    .is_err()
            );
            assert!(fragments.is_empty());
        }
        let mut state = StreamState::default();
        let event =
            json!({"choices": [{"delta": {"content": " "}, "finish_reason": "stop"}]}).to_string();
        let _ = state.handle(&event, &mut |_| Ok(())).unwrap();
        assert!(state.handle("[DONE]", &mut |_| Ok(())).is_err());
    }

    #[test]
    fn rejects_empty_refused_and_truncated_output() {
        for value in [
            json!({"choices": []}),
            json!({"choices": [{"message": {"content": null}, "finish_reason": "stop"}]}),
            json!({"choices": [{"message": {"content": "partial"}, "finish_reason": "length"}]}),
        ] {
            assert!(
                serde_json::from_value::<Response>(value)
                    .unwrap()
                    .markdown()
                    .is_err()
            );
        }
        let response: Response = serde_json::from_value(
            json!({"choices": [{"message": {"content": "# Bonjour\n"}, "finish_reason": "stop"}]}),
        )
        .unwrap();
        assert_eq!(response.markdown().unwrap(), "# Bonjour\n");
    }
}
