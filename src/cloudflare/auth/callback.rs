//! OAuth callbacks preserve provider failures without exposing callback secrets.
mod page;
use crate::error::{CodedError, ErrorCode};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use page::{PageState, page};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

pub(super) async fn listen(redirects: &[&str]) -> anyhow::Result<tokio::net::TcpListener> {
    for redirect in redirects {
        let uri = url::Url::parse(redirect)?;
        let port = uri.port_or_known_default();
        if !redirect.starts_with("http://127.0.0.1:")
            || uri.scheme() != "http"
            || uri.host_str() != Some("127.0.0.1")
            || !uri.username().is_empty()
            || uri.password().is_some()
            || uri.path() != "/oauth/callback"
            || uri.query().is_some()
            || uri.fragment().is_some()
            || port.is_none_or(|port| port == 0)
        {
            return Err(ErrorCode::InvalidConfig
                .error("invalid Cloudflare loopback callback URI")
                .into());
        }
        let address = (std::net::Ipv4Addr::LOCALHOST, port.unwrap());
        match tokio::net::TcpListener::bind(address).await {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(error) => {
                return Err(anyhow::Error::new(error).context(
                    ErrorCode::InvalidPort
                        .error(format!("could not bind Cloudflare callback {redirect}")),
                ));
            }
        }
    }
    Err(ErrorCode::InvalidPort.error(format!(
        "Cloudflare callback ports are all occupied; close another authorization and retry ({})",
        redirects.join(", ")
    )).into())
}

type CodeResult = Result<String, CodedError>;
pub(super) struct Callback {
    pub(super) state: String,
    pub(super) sender: Mutex<Option<oneshot::Sender<CodeResult>>>,
}

pub(super) async fn receive(
    State(callback): State<Arc<Callback>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let chinese = headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().to_ascii_lowercase().starts_with("zh"));
    if query.get("state") != Some(&callback.state) {
        return response(
            StatusCode::BAD_REQUEST,
            PageState::Invalid,
            chinese,
            Some("Invalid login state."),
        );
    }
    let code = if query.contains_key("error") {
        Err(failure(&query))
    } else if let Some(code) = query
        .get("code")
        .filter(|code| !code.is_empty() && code.len() <= 4096)
    {
        Ok(code.clone())
    } else {
        return response(
            StatusCode::BAD_REQUEST,
            PageState::Invalid,
            chinese,
            Some("Missing authorization code."),
        );
    };
    let detail = code.as_ref().err().map(ToString::to_string);
    let state = if code.is_ok() {
        PageState::Received
    } else {
        PageState::Declined
    };
    let Some(sender) = callback.sender.lock().expect("callback lock").take() else {
        return response(StatusCode::GONE, PageState::Finished, chinese, None);
    };
    let _ = sender.send(code);
    response(StatusCode::OK, state, chinese, detail.as_deref())
}

fn response(status: StatusCode, state: PageState, chinese: bool, detail: Option<&str>) -> Response {
    (
        status,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        page(state, chinese, detail),
    )
        .into_response()
}

fn failure(query: &HashMap<String, String>) -> CodedError {
    let name = query
        .get("error")
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 128
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
        .map_or("invalid_error_response", String::as_str);
    let (code, message) = match name {
        "invalid_scope" => (
            ErrorCode::OAuthInvalidScope,
            "Cloudflare OAuth client does not allow the requested permissions; update its registered scopes",
        ),
        "access_denied" => (
            ErrorCode::Unauthenticated,
            "Cloudflare authorization was declined",
        ),
        "invalid_client" | "unauthorized_client" | "invalid_request" => (
            ErrorCode::InvalidConfig,
            "Cloudflare OAuth client configuration is invalid",
        ),
        _ => (
            ErrorCode::Unauthenticated,
            "Cloudflare authorization failed",
        ),
    };
    let mut detail = format!("{message} ({name})");
    if let Some(description) = query
        .get("error_description")
        .filter(|value| !value.is_empty())
    {
        detail.push_str(": ");
        detail.push_str(description);
    }
    for key in [
        "state",
        "code",
        "access_token",
        "refresh_token",
        "id_token",
        "code_verifier",
        "client_secret",
    ] {
        if let Some(secret) = query.get(key).filter(|value| !value.is_empty()) {
            detail = detail.replace(secret, "[redacted]");
        }
    }
    code.error(
        detail
            .chars()
            .filter(|ch| !ch.is_control())
            .take(2048)
            .collect::<String>(),
    )
}

#[cfg(test)]
mod tests;
