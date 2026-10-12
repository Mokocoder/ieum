use std::sync::OnceLock;
use std::time::Duration;

use ureq::Agent;

use crate::diagnostic::{Diagnostic, Result, Span};
use crate::limits::MAX_TEXT_BYTES;

fn agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

fn request_error(span: Span, error: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::new("E-NET-001", span, format!("요청하지 못했습니다: {error}"))
}

pub(super) fn get(url: &str, span: Span) -> Result<String> {
    let response = agent()
        .get(url)
        .call()
        .map_err(|e| request_error(span, e))?;
    body(response, span)
}

pub(super) fn post(url: &str, body_text: String, json: bool, span: Span) -> Result<String> {
    let content = if json {
        "application/json; charset=utf-8"
    } else {
        "text/plain; charset=utf-8"
    };
    let response = agent()
        .post(url)
        .header("Content-Type", content)
        .send(body_text)
        .map_err(|e| request_error(span, e))?;
    body(response, span)
}

fn body(mut response: ureq::http::Response<ureq::Body>, span: Span) -> Result<String> {
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Diagnostic::new(
            "E-NET-002",
            span,
            format!("요청이 실패했습니다(상태 코드 {status})."),
        ));
    }
    let text = response
        .body_mut()
        .with_config()
        .limit(MAX_TEXT_BYTES as u64)
        .read_to_string()
        .map_err(|e| request_error(span, e))?;
    Ok(text)
}
