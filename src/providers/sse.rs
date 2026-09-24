use std::ops::ControlFlow;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::translate::TranslationError;

const MAX_EVENT_BYTES: usize = 1024 * 1024;

// Read incrementally with the existing reqwest client: no streaming SDK, retries,
// reconnects, or buffering of the whole document. Completion is provider-specific.
pub(super) async fn consume(
    request: reqwest::RequestBuilder,
    mut on_event: impl FnMut(&str) -> Result<ControlFlow<()>, TranslationError> + Send,
) -> Result<(), TranslationError> {
    let mut response = super::send(request.header("accept", "text/event-stream")).await?;
    let is_sse = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
    if !is_sse {
        return Err(TranslationError::InvalidResponse(
            "expected text/event-stream; try --no-stream if the provider does not support streaming",
        ));
    }
    let mut decoder = Decoder::new();
    while let Some(chunk) = response.chunk().await.map_err(super::transport)? {
        if decoder.feed(&chunk, &mut on_event)?.is_break() {
            return Ok(());
        }
    }
    // SSE does not dispatch an unterminated event at EOF. Even a well-framed
    // stream must contain the provider's explicit successful completion marker.
    Err(TranslationError::InvalidResponse(
        "stream ended before successful completion",
    ))
}

// Error payloads must never be echoed: they can contain keys or document text.
// Recognize only HTTP codes and known machine-readable quota/auth identifiers.
pub(super) fn json<T: DeserializeOwned>(data: &str) -> Result<T, TranslationError> {
    let value: Value = serde_json::from_str(data)
        .map_err(|_| TranslationError::InvalidResponse("invalid JSON in stream"))?;
    if let Some(error) = value.get("error") {
        if let Some(code) = error
            .get("code")
            .and_then(Value::as_u64)
            .filter(|code| (400..600).contains(code))
        {
            return Err(TranslationError::Http(code as u16));
        }
        for field in ["code", "type", "status"] {
            let status = match error.get(field).and_then(Value::as_str) {
                Some("insufficient_quota" | "rate_limit_exceeded" | "RESOURCE_EXHAUSTED") => {
                    Some(429)
                }
                Some("UNAUTHENTICATED" | "invalid_api_key") => Some(401),
                Some("PERMISSION_DENIED") => Some(403),
                Some("NOT_FOUND") => Some(404),
                Some("UNAVAILABLE") => Some(503),
                _ => None,
            };
            if let Some(status) = status {
                return Err(TranslationError::Http(status));
            }
        }
        return Err(TranslationError::InvalidResponse(
            "provider reported an error during streaming",
        ));
    }
    serde_json::from_value(value)
        .map_err(|_| TranslationError::InvalidResponse("unexpected stream response schema"))
}

struct Decoder {
    line: Vec<u8>,
    data: String,
    has_data: bool,
    first_line: bool,
    skip_lf: bool,
}

impl Decoder {
    fn new() -> Self {
        Self {
            line: Vec::new(),
            data: String::new(),
            has_data: false,
            first_line: true,
            skip_lf: false,
        }
    }

    // TCP/HTTP chunks are not SSE events or even complete UTF-8 characters.
    // Follow SSE line framing: LF, CRLF, and CR; ignore an initial UTF-8 BOM.
    fn feed(
        &mut self,
        bytes: &[u8],
        on_event: &mut impl FnMut(&str) -> Result<ControlFlow<()>, TranslationError>,
    ) -> Result<ControlFlow<()>, TranslationError> {
        for &byte in bytes {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if matches!(byte, b'\r' | b'\n') {
                self.skip_lf = byte == b'\r';
                if self.finish_line(on_event)?.is_break() {
                    return Ok(ControlFlow::Break(()));
                }
            } else {
                if self.line.len() + self.data.len() >= MAX_EVENT_BYTES {
                    return Err(TranslationError::InvalidResponse(
                        "stream event exceeds the 1 MiB limit",
                    ));
                }
                self.line.push(byte);
            }
        }
        Ok(ControlFlow::Continue(()))
    }

    fn finish_line(
        &mut self,
        on_event: &mut impl FnMut(&str) -> Result<ControlFlow<()>, TranslationError>,
    ) -> Result<ControlFlow<()>, TranslationError> {
        let bytes = std::mem::take(&mut self.line);
        let mut line = std::str::from_utf8(&bytes)
            .map_err(|_| TranslationError::InvalidResponse("invalid UTF-8 in stream"))?;
        if self.first_line {
            self.first_line = false;
            line = line.strip_prefix('\u{feff}').unwrap_or(line);
        }
        if line.is_empty() {
            let result = if self.has_data {
                on_event(&self.data)?
            } else {
                ControlFlow::Continue(())
            };
            self.data.clear();
            self.has_data = false;
            return Ok(result);
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        if field == "data" {
            if self.has_data {
                self.data.push('\n');
            }
            self.data.push_str(value.strip_prefix(' ').unwrap_or(value));
            self.has_data = true;
        }
        // Comments, event names, IDs, and retry hints do not carry Markdown.
        Ok(ControlFlow::Continue(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_split_utf8_bom_crlf_comments_and_multiline_data() {
        let input = "\u{feff}: heartbeat\r\nid: 7\revent: message\r\nretry: 100\r\ndata:  Héllo\r\ndata: 世界\r\n\r\ndata: next\n\ndata\n\n";
        for chunk_size in 1..=input.len() {
            let mut decoder = Decoder::new();
            let mut events = Vec::new();
            for chunk in input.as_bytes().chunks(chunk_size) {
                let _ = decoder
                    .feed(chunk, &mut |data| {
                        events.push(data.to_owned());
                        Ok(ControlFlow::Continue(()))
                    })
                    .unwrap();
            }
            assert_eq!(events, [" Héllo\n世界", "next", ""]);
        }
    }

    #[test]
    fn does_not_dispatch_an_unterminated_event_and_stops_on_completion() {
        let mut decoder = Decoder::new();
        let mut events = Vec::new();
        let mut collect = |data: &str| {
            events.push(data.to_owned());
            Ok(ControlFlow::Continue(()))
        };
        assert!(
            decoder
                .feed(b"data: pending\n", &mut collect)
                .unwrap()
                .is_continue()
        );
        assert!(events.is_empty());
        assert!(
            decoder
                .feed(b"\ndata: unread\n\n", &mut |_| Ok(ControlFlow::Break(())))
                .unwrap()
                .is_break()
        );
    }

    #[test]
    fn rejects_invalid_utf8_and_oversized_events() {
        let mut callback = |_: &str| Ok(ControlFlow::Continue(()));
        assert!(
            Decoder::new()
                .feed(b"data: \xff\n\n", &mut callback)
                .is_err()
        );
        assert!(
            Decoder::new()
                .feed(&vec![b'x'; MAX_EVENT_BYTES + 1], &mut callback)
                .is_err()
        );
    }

    #[test]
    fn classifies_in_band_errors_without_leaking_payloads() {
        for data in [
            r#"{"error":{"code":429,"message":"fake-secret"}}"#,
            r#"{"error":{"code":"insufficient_quota","message":"fake-secret"}}"#,
            r#"{"error":{"status":"RESOURCE_EXHAUSTED","message":"fake-secret"}}"#,
        ] {
            assert!(matches!(
                json::<Value>(data),
                Err(TranslationError::Http(429))
            ));
        }
        let error = json::<Value>(r#"{"error":{"message":"fake-secret"}}"#).unwrap_err();
        assert!(!error.to_string().contains("fake-secret"));
        assert!(json::<Value>("not json").is_err());
    }
}
