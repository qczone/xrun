use super::state;
use crate::{config, error::ErrorCode};
use anyhow::{Context, Result, bail};
use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;
use url::Url;

const REDIRECT: &str = "http://127.0.0.1:8976/oauth/callback";
const AUTHORIZE: &str = "https://dash.cloudflare.com/oauth2/auth";
const TOKEN: &str = "https://dash.cloudflare.com/oauth2/token";

#[derive(Deserialize, Serialize)]
struct Credentials {
    client_id: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: u64,
}

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    token_type: String,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn client_id() -> Result<String> {
    let value = std::env::var("XRUN_CLOUDFLARE_OAUTH_CLIENT_ID")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| env!("XRUN_CLOUDFLARE_PUBLIC_CLIENT_ID").to_owned());
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!(
            ErrorCode::InvalidConfig
                .error("Cloudflare OAuth client ID must be 32 hexadecimal characters")
        )
    }
    Ok(value)
}

pub(super) fn configured() -> bool {
    client_id().is_ok()
}

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()?)
}

pub(super) async fn token(no_browser: bool) -> Result<String> {
    token_with_prompt(no_browser, None).await
}

pub(super) async fn token_with_prompt(
    no_browser: bool,
    prompt: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> Result<String> {
    if let Ok(value) = std::env::var("CLOUDFLARE_API_TOKEN") {
        if value.is_empty() {
            bail!(ErrorCode::InvalidToken.error("CLOUDFLARE_API_TOKEN is empty"))
        }
        return Ok(value);
    }
    let directory = state::directory()?;
    let _lock = state::lock(&directory, "credentials")?;
    let client_id = client_id()?;
    if let Some(token) = cached_token(&directory, &client_id, &http()?, TOKEN).await? {
        return Ok(token);
    }
    authorize_with_prompt(&directory, &client_id, no_browser, prompt).await
}

async fn cached_token(
    directory: &Path,
    client_id: &str,
    client: &reqwest::Client,
    endpoint: &str,
) -> Result<Option<String>> {
    let path = directory.join("credentials.toml");
    if let Some(mut credentials) = load(&path)?
        && credentials.client_id == client_id
    {
        if credentials.expires_at > now() + 60 {
            return Ok(Some(credentials.access_token));
        }
        if let Some(refresh) = &credentials.refresh_token {
            match exchange(
                client,
                endpoint,
                &[
                    ("grant_type", "refresh_token"),
                    ("client_id", client_id),
                    ("refresh_token", refresh),
                ],
            )
            .await?
            {
                Some(tokens) => {
                    credentials = credentials.with_tokens(tokens)?;
                    save(&path, &credentials)?;
                    return Ok(Some(credentials.access_token));
                }
                None => {
                    std::fs::remove_file(&path)?;
                }
            }
        }
    }
    Ok(None)
}

pub(crate) async fn login(no_browser: bool) -> Result<()> {
    login_with_prompt(no_browser, None).await?;
    Ok(())
}

pub(super) async fn login_with_prompt(
    no_browser: bool,
    prompt: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> Result<String> {
    let directory = state::directory()?;
    let _lock = state::lock(&directory, "credentials")?;
    authorize_with_prompt(&directory, &client_id()?, no_browser, prompt).await
}

pub(crate) async fn logout() -> Result<()> {
    let directory = state::directory()?;
    let _lock = state::lock(&directory, "credentials")?;
    let path = directory.join("credentials.toml");
    if let Some(credentials) = load(&path)? {
        let token = credentials
            .refresh_token
            .as_deref()
            .unwrap_or(&credentials.access_token);
        let status = form(
            &http()?,
            "https://dash.cloudflare.com/oauth2/revoke",
            &[
                ("client_id", credentials.client_id.as_str()),
                ("token", token),
            ],
        )
        .send()
        .await?
        .status();
        if !status.is_success() {
            bail!(ErrorCode::HttpError.error(format!(
                "Cloudflare token revocation HTTP {status}; login retained"
            )))
        }
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn load(path: &Path) -> Result<Option<Credentials>> {
    match std::fs::read_to_string(path) {
        Ok(source) => Ok(Some(toml::from_str(&source).map_err(|_| {
            ErrorCode::InvalidConfig.error(
                "invalid saved Cloudflare login; run xrun relay login --provider cloudflare again",
            )
        })?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn save(path: &Path, credentials: &Credentials) -> Result<()> {
    config::atomic_private_write(path, toml::to_string(credentials)?.as_bytes())
}

impl Credentials {
    fn with_tokens(self, tokens: Tokens) -> Result<Self> {
        if tokens.access_token.is_empty()
            || !tokens.token_type.eq_ignore_ascii_case("bearer")
            || tokens.expires_in == 0
        {
            bail!(ErrorCode::InvalidToken.error("Cloudflare returned invalid OAuth credentials"))
        }
        Ok(Self {
            client_id: self.client_id,
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token.or(self.refresh_token),
            expires_at: now().saturating_add(tokens.expires_in),
        })
    }
}

async fn exchange(
    client: &reqwest::Client,
    endpoint: &str,
    fields: &[(&str, &str)],
) -> Result<Option<Tokens>> {
    let mut response = form(client, endpoint, fields)
        .send()
        .await
        .context("exchange Cloudflare authorization")?;
    let status = response.status();
    // OAuth responses can contain credentials. Never include their body in diagnostics.
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > 64 * 1024 {
            bail!(ErrorCode::InvalidBody.error("Cloudflare OAuth response exceeds 64 KiB"))
        }
        bytes.extend_from_slice(&chunk);
    }
    if status == StatusCode::BAD_REQUEST
        && serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .is_some_and(|value| value["error"] == "invalid_grant")
    {
        return Ok(None);
    }
    if !status.is_success() {
        bail!(ErrorCode::Unauthenticated.error(format!("Cloudflare OAuth HTTP {status}")))
    }
    Ok(Some(serde_json::from_slice(&bytes).map_err(|_| {
        ErrorCode::InvalidBody.error("invalid Cloudflare OAuth token response")
    })?))
}

fn form(
    client: &reqwest::Client,
    endpoint: &str,
    fields: &[(&str, &str)],
) -> reqwest::RequestBuilder {
    client
        .post(endpoint)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(
            url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(fields)
                .finish(),
        )
}

struct Authorization {
    state: String,
    verifier: String,
}

impl Authorization {
    fn new() -> Result<Self> {
        fn random() -> Result<String> {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes)
                .map_err(|error| anyhow::anyhow!("generate OAuth nonce: {error}"))?;
            Ok(URL_SAFE_NO_PAD.encode(bytes))
        }
        Ok(Self {
            state: random()?,
            verifier: random()?,
        })
    }

    fn url(&self, client_id: &str) -> Result<Url> {
        let mut url = Url::parse(AUTHORIZE)?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", REDIRECT),
            ("state", &self.state),
            (
                "scope",
                "account-settings.read workers-scripts.write workers-routes.write offline_access",
            ),
            ("code_challenge_method", "S256"),
            (
                "code_challenge",
                &URL_SAFE_NO_PAD.encode(Sha256::digest(self.verifier.as_bytes())),
            ),
        ]);
        Ok(url)
    }
}

type CodeResult = std::result::Result<String, String>;
struct Callback {
    state: String,
    sender: Mutex<Option<oneshot::Sender<CodeResult>>>,
}

async fn callback(
    State(callback): State<Arc<Callback>>,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if query.get("state") != Some(&callback.state) {
        return (StatusCode::BAD_REQUEST, Html("Invalid login state."));
    }
    let code = if query.contains_key("error") {
        Err("Cloudflare authorization was declined".into())
    } else if let Some(code) = query
        .get("code")
        .filter(|code| !code.is_empty() && code.len() <= 4096)
    {
        Ok(code.clone())
    } else {
        return (StatusCode::BAD_REQUEST, Html("Missing authorization code."));
    };
    let success = code.is_ok();
    let Some(sender) = callback.sender.lock().expect("callback lock").take() else {
        return (StatusCode::GONE, Html("This login has already completed."));
    };
    let _ = sender.send(code);
    (
        StatusCode::OK,
        Html(if success {
            concat!(
                "<!doctype html><meta name=referrer content=no-referrer><title>xrun</title>",
                "<script>history.replaceState(null,'','/')</script>Authorized. Return to xrun to finish deployment."
            )
        } else {
            "Authorization declined. Return to xrun."
        }),
    )
}

struct CallbackServer(tokio::task::AbortHandle);
impl Drop for CallbackServer {
    fn drop(&mut self) {
        // Dropping a desktop authorization future must release the listener too.
        self.0.abort();
    }
}

async fn authorize_with_prompt(
    directory: &Path,
    client_id: &str,
    no_browser: bool,
    prompt: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8976")
        .await
        .context(
            ErrorCode::InvalidPort
                .error("Cloudflare login needs local port 8976; close another login and retry"),
        )?;
    authorize_with_listener(directory, client_id, no_browser, prompt, listener).await
}

async fn authorize_with_listener(
    directory: &Path,
    client_id: &str,
    no_browser: bool,
    prompt: Option<&(dyn Fn(&str) + Send + Sync)>,
    listener: tokio::net::TcpListener,
) -> Result<String> {
    let authorization = Authorization::new()?;
    let (sender, receiver) = oneshot::channel();
    let callback = Arc::new(Callback {
        state: authorization.state.clone(),
        sender: Mutex::new(Some(sender)),
    });
    let router = Router::new()
        .route("/oauth/callback", get(self::callback))
        .with_state(callback);
    let (shutdown, stopped) = oneshot::channel();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    let _server_guard = CallbackServer(server.abort_handle());
    let url = authorization.url(client_id)?;
    if let Some(prompt) = prompt {
        prompt(url.as_str());
    } else {
        eprintln!("[xrun] Authorize xrun with Cloudflare: {url}");
    }
    if !no_browser {
        open_browser(url.as_str());
    }
    let result = tokio::select! {
        code = receiver => code.context("Cloudflare login callback closed")
            .and_then(|code| code.map_err(|message| ErrorCode::Unauthenticated.error(message).into())),
        _ = tokio::time::sleep(Duration::from_secs(300)) => Err(ErrorCode::ConnectTimeout.error("Cloudflare login timed out").into()),
        _ = tokio::signal::ctrl_c() => Err(ErrorCode::Unauthenticated.error("Cloudflare login cancelled").into()),
    };
    let _ = shutdown.send(());
    if tokio::time::timeout(Duration::from_secs(3), &mut server)
        .await
        .is_err()
    {
        server.abort();
    }
    let code = result?;
    let tokens = exchange(
        &http()?,
        TOKEN,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("redirect_uri", REDIRECT),
            ("code", &code),
            ("code_verifier", &authorization.verifier),
        ],
    )
    .await?
    .context(
        ErrorCode::Unauthenticated.error("Cloudflare authorization code expired; retry login"),
    )?;
    let credentials = Credentials {
        client_id: client_id.to_owned(),
        access_token: String::new(),
        refresh_token: None,
        expires_at: 0,
    }
    .with_tokens(tokens)?;
    save(&directory.join("credentials.toml"), &credentials)?;
    Ok(credentials.access_token)
}

pub(crate) fn reopen_browser(value: &str) -> Result<()> {
    let url = Url::parse(value)?;
    if url.scheme() != "https"
        || url.host_str() != Some("dash.cloudflare.com")
        || url.path() != "/oauth2/auth"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        bail!(ErrorCode::InvalidLink.error("expected the Cloudflare authorization address"));
    }
    open_browser(value);
    Ok(())
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = std::process::Command::new("explorer.exe");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if command.spawn().is_err() {
        eprintln!("[xrun] Open the authorization address in your browser.");
    }
}

#[cfg(test)]
mod tests;
