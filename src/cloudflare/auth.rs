mod vault;
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
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;
use url::Url;
use vault::{SystemVault, Vault};

const REDIRECT: &str = "http://127.0.0.1:8976/oauth/callback";
const AUTHORIZE: &str = "https://dash.cloudflare.com/oauth2/auth";
const TOKEN: &str = "https://dash.cloudflare.com/oauth2/token";

#[derive(Clone, Deserialize, Serialize)]
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

const REVOKE: &str = "https://dash.cloudflare.com/oauth2/revoke";
const TEMPORARY_LIFETIME: Duration = Duration::from_secs(300);
static TEMPORARY: OnceLock<Mutex<Option<(String, Credentials)>>> = OnceLock::new();
type Cancellation = Option<(String, oneshot::Sender<()>)>;
static PENDING: OnceLock<Mutex<Cancellation>> = OnceLock::new();
fn pending() -> &'static Mutex<Cancellation> {
    PENDING.get_or_init(|| Mutex::new(None))
}
struct PendingLogin(String);
impl Drop for PendingLogin {
    fn drop(&mut self) {
        let mut saved = pending().lock().unwrap();
        if saved.as_ref().is_some_and(|(stamp, _)| stamp == &self.0) {
            saved.take();
        }
    }
}
pub(crate) fn cancel_pending_login() -> bool {
    pending()
        .lock()
        .unwrap()
        .take()
        .is_some_and(|(_, cancel)| cancel.send(()).is_ok())
}
fn temporary() -> &'static Mutex<Option<(String, Credentials)>> {
    TEMPORARY.get_or_init(|| Mutex::new(None))
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LoginPreference {
    remembered: bool,
}
fn remembered(directory: &Path) -> Result<bool> {
    let path = directory.join("login.toml");
    if !path.exists() {
        return Ok(false);
    }
    Ok(config::read::<LoginPreference>(&path)?.remembered)
}
fn require_manager() -> Result<()> {
    let id = crate::config::Identity::load().context(ErrorCode::NotManager.error(
        "remembered Cloudflare login requires a manager device; use temporary deployment first",
    ))?;
    if crate::network::current(&id)?.roster.manager_id != id.device_id {
        bail!(
            ErrorCode::NotManager
                .error("remembered Cloudflare login requires a protected manager device")
        );
    }
    crate::network::manager(&id)?;
    Ok(())
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
            bail!(ErrorCode::InvalidToken.error("CLOUDFLARE_API_TOKEN is empty"));
        }
        return Ok(value);
    }
    let directory = state::directory()?;
    let _lock = state::lock(&directory, "credentials")?;
    cleanup_legacy(&directory).await?;
    let client_id = client_id()?;
    let remember = remembered(&directory)?;
    if remember {
        require_manager()?;
        let store = SystemVault::open(&directory, &client_id)?;
        if let Some(token) = cached_token(&store, &client_id, &http()?, TOKEN).await? {
            return Ok(token);
        }
    }
    if let Some((_, credentials)) = temporary().lock().unwrap().as_ref()
        && credentials.client_id == client_id
        && credentials.expires_at > now() + 60
    {
        return Ok(credentials.access_token.clone());
    }
    finish_temporary(Ok(())).await?;
    authorize_with_prompt(&directory, &client_id, remember, no_browser, prompt).await
}
async fn cached_token(
    store: &impl Vault,
    client_id: &str,
    client: &reqwest::Client,
    endpoint: &str,
) -> Result<Option<String>> {
    if let Some(mut credentials) = store.load()?
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
                    store.save(&credentials)?;
                    return Ok(Some(credentials.access_token));
                }
                None => {
                    store.delete()?;
                }
            }
        }
    }
    Ok(None)
}
pub(crate) async fn login(no_browser: bool, remember: bool) -> Result<()> {
    login_with_prompt(no_browser, remember, None).await?;
    if !remember {
        finish_temporary(Ok(())).await?;
    }
    Ok(())
}
pub(super) async fn login_with_prompt(
    no_browser: bool,
    remember: bool,
    prompt: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> Result<String> {
    let directory = state::directory()?;
    let _lock = state::lock(&directory, "credentials")?;
    if remember {
        require_manager()?;
        SystemVault::open(&directory, &client_id()?)?;
    }
    finish_temporary(Ok(())).await?;
    logout_saved(&directory).await?;
    cleanup_legacy(&directory).await?;
    authorize_with_prompt(&directory, &client_id()?, remember, no_browser, prompt).await
}
async fn revoke_credentials(
    client: &reqwest::Client,
    endpoint: &str,
    credentials: &Credentials,
) -> Result<()> {
    // Revoke a refresh grant as well as its current access token, when supplied.
    for token in credentials
        .refresh_token
        .iter()
        .chain(std::iter::once(&credentials.access_token))
    {
        let response = form(
            client,
            endpoint,
            &[("client_id", &credentials.client_id), ("token", token)],
        )
        .send()
        .await
        .map_err(|_| {
            ErrorCode::OAuthRevocationFailed
                .error("Cloudflare revocation could not be confirmed; retry logout")
        })?;
        if !response.status().is_success() {
            bail!(ErrorCode::OAuthRevocationFailed.error(format!(
                "Cloudflare revocation HTTP {}; retry logout",
                response.status()
            )));
        }
    }
    Ok(())
}
pub(crate) async fn finish_temporary<T>(result: Result<T>) -> Result<T> {
    finish_temporary_for(None, result).await
}
async fn finish_temporary_for<T>(expected: Option<&str>, result: Result<T>) -> Result<T> {
    finish_temporary_at(expected, &http()?, REVOKE, result).await
}
async fn finish_temporary_at<T>(
    expected: Option<&str>,
    client: &reqwest::Client,
    endpoint: &str,
    result: Result<T>,
) -> Result<T> {
    let current = temporary()
        .lock()
        .unwrap()
        .as_ref()
        .filter(|(stamp, _)| expected.is_none_or(|expected| expected == stamp))
        .cloned();
    if let Some((stamp, credentials)) = current {
        if let Err(error) = revoke_credentials(client, endpoint, &credentials).await {
            return Err(error.context(if result.is_ok() {
                "operation completed; temporary authorization revocation is unconfirmed"
            } else {
                "operation failed and temporary authorization revocation is unconfirmed"
            }));
        }
        let mut saved = temporary().lock().unwrap();
        if saved.as_ref().is_some_and(|(current, _)| current == &stamp) {
            saved.take();
        }
    }
    result
}
fn save_temporary(mut credentials: Credentials) {
    credentials.expires_at = credentials
        .expires_at
        .min(now() + TEMPORARY_LIFETIME.as_secs());
    let stamp = uuid::Uuid::new_v4().to_string();
    *temporary().lock().unwrap() = Some((stamp.clone(), credentials));
    tokio::spawn(async move {
        tokio::time::sleep(TEMPORARY_LIFETIME).await;
        if let Err(error) = finish_temporary_for(Some(&stamp), Ok(())).await {
            tracing::warn!(code=?crate::error::code(&error), "temporary Cloudflare revocation remains unconfirmed");
        }
    });
}
// Only used to retire the old plaintext format, never to authenticate an operation.
async fn cleanup_legacy(directory: &Path) -> Result<()> {
    let path = directory.join("credentials.toml");
    if path.exists() {
        let source = std::fs::read_to_string(&path)?;
        let old: Credentials = toml::from_str(&source).map_err(|_| {
            ErrorCode::InvalidConfig
                .error("invalid legacy Cloudflare login; revoke it in Cloudflare")
        })?;
        revoke_credentials(&http()?, REVOKE, &old).await?;
        std::fs::remove_file(&path)?;
        config::sync_parent(&path)?;
    }
    Ok(())
}
pub(crate) async fn logout() -> Result<()> {
    let directory = state::directory()?;
    let _lock = state::lock(&directory, "credentials")?;
    finish_temporary(Ok(())).await?;
    cleanup_legacy(&directory).await?;
    logout_saved(&directory).await
}
async fn logout_saved(directory: &Path) -> Result<()> {
    if remembered(directory)? {
        let store = SystemVault::open(directory, &client_id()?)?;
        revoke_saved(&store, &http()?, REVOKE).await?;
        config::write(
            &directory.join("login.toml"),
            &LoginPreference { remembered: false },
        )?;
    }
    Ok(())
}

async fn revoke_saved(store: &impl Vault, client: &reqwest::Client, endpoint: &str) -> Result<()> {
    if let Some(credentials) = store.load()? {
        revoke_credentials(client, endpoint, &credentials).await?;
    }
    store.delete()
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

    fn url(&self, client_id: &str, remember: bool) -> Result<Url> {
        let mut url = Url::parse(AUTHORIZE)?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", REDIRECT),
            ("state", &self.state),
            (
                "scope",
                if remember {
                    "account-settings.read workers-scripts.write offline_access"
                } else {
                    "account-settings.read workers-scripts.write"
                },
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
    remember: bool,
    no_browser: bool,
    prompt: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8976")
        .await
        .context(
            ErrorCode::InvalidPort
                .error("Cloudflare login needs local port 8976; close another login and retry"),
        )?;
    let (cancel, cancelled) = oneshot::channel();
    let stamp = uuid::Uuid::new_v4().to_string();
    *pending().lock().unwrap() = Some((stamp.clone(), cancel));
    let _pending = PendingLogin(stamp);
    tokio::select! {
        biased;
        _ = cancelled => {
            // Cancellation only drops the OAuth flow. It cannot abort a Worker
            // upload that started after this guard was retired.
            logout_saved(directory).await?;
            finish_temporary(Err(ErrorCode::Unauthenticated.error("Cloudflare authorization cancelled").into())).await
        },
        result = authorize_with_listener(directory, client_id, remember, no_browser, prompt, listener) => result,
    }
}

async fn authorize_with_listener(
    directory: &Path,
    client_id: &str,
    remember: bool,
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
    let url = authorization.url(client_id, remember)?;
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
    let token = credentials.access_token.clone();
    if remember {
        require_manager()?;
        let store = SystemVault::open(directory, client_id)?;
        if let Err(error) = config::write(
            &directory.join("login.toml"),
            &LoginPreference { remembered: true },
        )
        .and_then(|()| store.save(&credentials))
        {
            match revoke_credentials(&http()?, REVOKE, &credentials).await {
                Ok(()) => {
                    let _ = store.delete();
                    let _ = config::write(
                        &directory.join("login.toml"),
                        &LoginPreference { remembered: false },
                    );
                }
                Err(error) => {
                    let mut retirement = credentials;
                    retirement.expires_at = 0; // Retry revocation only; never authenticate from this failure.
                    save_temporary(retirement);
                    return Err(error.context("remembered login could not be saved or revoked"));
                }
            }
            return Err(error.context("remembered login was not saved"));
        }
    } else {
        save_temporary(credentials);
    }
    Ok(token)
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

pub(crate) fn remembered_login() -> Result<bool> {
    remembered(&config::device_dir()?.join("cloudflare"))
}
