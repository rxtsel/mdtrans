pub mod gemini;
pub mod openai_compatible;

use std::time::Duration;

use crate::translate::TranslationError;

pub fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(15))
        // Do not forward credentials or documents to a redirected endpoint.
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

fn transport(error: reqwest::Error) -> TranslationError {
    TranslationError::Transport(Box::new(error.without_url()))
}

async fn send_json<T: serde::de::DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> Result<T, TranslationError> {
    let response = request.send().await.map_err(transport)?;
    if !response.status().is_success() {
        // Response bodies may contain credentials or document contents.
        return Err(TranslationError::Http(response.status().as_u16()));
    }
    response
        .json()
        .await
        .map_err(|_| TranslationError::InvalidResponse("expected provider JSON schema"))
}
