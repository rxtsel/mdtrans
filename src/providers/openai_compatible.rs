use std::{future::Future, pin::Pin};

use anyhow::{Context, Result, bail};
use reqwest::{
    Client, Url,
    header::{AUTHORIZATION, HeaderValue},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::translate::{TranslationError, TranslationRequest, Translator};

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
