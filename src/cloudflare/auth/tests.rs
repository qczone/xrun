use super::*;
use axum::{Json, body::Body, http::Request};
use serde_json::json;
use tower::ServiceExt;

#[tokio::test]
async fn cancelling_desktop_authorization_releases_its_callback_listener() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (prompted, wait_prompt) = oneshot::channel();
    let sender = Mutex::new(Some(prompted));
    let path = directory.path().to_path_buf();
    let task = tokio::spawn(async move {
        let prompt = |_: &str| {
            let _ = sender.lock().unwrap().take().unwrap().send(());
        };
        authorize_with_listener(
            &path,
            "0123456789abcdef0123456789abcdef",
            false,
            true,
            Some(&prompt),
            listener,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), wait_prompt).await??;
    task.abort();
    let _ = task.await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(listener) = tokio::net::TcpListener::bind(address).await {
                break listener;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert!(!directory.path().join("credentials.toml").exists());
    Ok(())
}

#[tokio::test]
async fn callbacks_reject_wrong_states_and_consume_valid_codes_once() -> Result<()> {
    let authorization = Authorization::new()?;
    let (sender, mut receiver) = oneshot::channel();
    let callback = Arc::new(Callback {
        state: authorization.state.clone(),
        sender: Mutex::new(Some(sender)),
    });
    let router = Router::new()
        .route("/oauth/callback", get(super::callback))
        .with_state(callback);
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/oauth/callback?state=wrong&code=fake")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(matches!(
        receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    let url = format!(
        "/oauth/callback?state={}&code=real-code",
        authorization.state
    );
    let response = router
        .clone()
        .oneshot(Request::builder().uri(&url).body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        receiver
            .await?
            .map_err(|message| anyhow::anyhow!(message))?,
        "real-code"
    );
    let response = router
        .oneshot(Request::builder().uri(&url).body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::GONE);
    let url = authorization.url("xrun-client", false)?;
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], "xrun-client");
    assert_eq!(query["redirect_uri"], REDIRECT);
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(
        query["scope"].contains("workers-scripts.write")
            && !query["scope"].contains("offline_access")
            && !query["scope"].contains("workers-routes.write")
    );
    assert_eq!(
        query["code_challenge"],
        URL_SAFE_NO_PAD.encode(Sha256::digest(authorization.verifier.as_bytes()))
    );
    assert!(url.as_str().starts_with(AUTHORIZE));
    assert!(
        authorization
            .url("xrun-client", true)?
            .query_pairs()
            .any(|(key, value)| key == "scope" && value.contains("offline_access"))
    );
    Ok(())
}

#[tokio::test]
async fn denied_consent_is_an_explicit_login_failure() -> Result<()> {
    let (sender, receiver) = oneshot::channel();
    let callback = Arc::new(Callback {
        state: "state".into(),
        sender: Mutex::new(Some(sender)),
    });
    let router = Router::new()
        .route("/oauth/callback", get(super::callback))
        .with_state(callback);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/oauth/callback?state=state&error=access_denied")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(receiver.await?.is_err());
    Ok(())
}

#[derive(Default)]
struct MemoryVault(Mutex<Option<String>>);
impl Vault for MemoryVault {
    fn load(&self) -> Result<Option<Credentials>> {
        self.0
            .lock()
            .unwrap()
            .as_deref()
            .map(vault::decode)
            .transpose()
    }
    fn save(&self, credentials: &Credentials) -> Result<()> {
        *self.0.lock().unwrap() = Some(serde_json::to_string(credentials)?);
        Ok(())
    }
    fn delete(&self) -> Result<()> {
        self.0.lock().unwrap().take();
        Ok(())
    }
}
fn expired() -> Credentials {
    Credentials {
        client_id: "xrun-client".into(),
        access_token: "expired".into(),
        refresh_token: Some("keep-secret".into()),
        expires_at: 0,
    }
}

#[tokio::test]
async fn temporary_tokens_are_revoked_on_failed_operations_and_old_timers_cannot_revoke_new_sessions()
-> Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/revoke", listener.local_addr()?);
    let router = Router::new().route("/revoke", axum::routing::post(|| async { StatusCode::OK }));
    let task = tokio::spawn(async move { axum::serve(listener, router).await });
    *temporary().lock().unwrap() = Some(("new-session".into(), expired()));
    finish_temporary_at(Some("old-session"), &http()?, &endpoint, Ok(())).await?;
    assert!(temporary().lock().unwrap().is_some());
    let result: Result<()> = Err(ErrorCode::HttpError
        .error("owned deployment fixture failed")
        .into());
    let result = finish_temporary_at(Some("new-session"), &http()?, &endpoint, result).await;
    assert!(crate::error::is(&result.unwrap_err(), ErrorCode::HttpError));
    assert!(temporary().lock().unwrap().is_none());
    task.abort();
    Ok(())
}

#[tokio::test]
async fn an_unavailable_store_never_falls_back_to_a_token_file() -> Result<()> {
    struct Unavailable;
    impl Vault for Unavailable {
        fn load(&self) -> Result<Option<Credentials>> {
            bail!(ErrorCode::CredentialStoreUnavailable.error("test store unavailable"));
        }
        fn save(&self, _: &Credentials) -> Result<()> {
            unreachable!()
        }
        fn delete(&self) -> Result<()> {
            unreachable!()
        }
    }
    let error = cached_token(&Unavailable, "xrun-client", &http()?, "http://127.0.0.1:1")
        .await
        .unwrap_err();
    assert!(crate::error::is(
        &error,
        ErrorCode::CredentialStoreUnavailable
    ));
    Ok(())
}
#[tokio::test]
async fn cached_login_refreshes_without_losing_the_refresh_token() -> Result<()> {
    let store = MemoryVault::default();
    store.save(&expired())?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/token", listener.local_addr()?);
    let router = Router::new().route(
        "/token",
        axum::routing::post(
            |axum::Form(form): axum::Form<HashMap<String, String>>| async move {
                assert_eq!(form["grant_type"], "refresh_token");
                assert_eq!(form["refresh_token"], "keep-secret");
                Json(json!({"access_token":"new-access","expires_in":3600,"token_type":"Bearer"}))
            },
        ),
    );
    let task = tokio::spawn(async move { axum::serve(listener, router).await });
    let result = cached_token(&store, "xrun-client", &http()?, &endpoint).await;
    task.abort();
    assert_eq!(result?.as_deref(), Some("new-access"));
    let saved = store.load()?.unwrap();
    assert_eq!(saved.refresh_token.as_deref(), Some("keep-secret"));
    assert!(saved.expires_at > now() + 3000);
    assert_eq!(
        cached_token(&store, "xrun-client", &http()?, "http://127.0.0.1:1")
            .await?
            .as_deref(),
        Some("new-access")
    );
    assert!(
        cached_token(&store, "different-client", &http()?, "http://127.0.0.1:1")
            .await?
            .is_none()
    );
    Ok(())
}
#[tokio::test]
async fn invalid_grants_require_login_but_transient_errors_preserve_credentials() -> Result<()> {
    for (status, error) in [
        (StatusCode::SERVICE_UNAVAILABLE, "transient"),
        (StatusCode::BAD_REQUEST, "invalid_grant"),
    ] {
        let store = MemoryVault::default();
        store.save(&expired())?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/token", listener.local_addr()?);
        let router = Router::new().route(
            "/token",
            axum::routing::post(move || async move {
                (status, Json(json!({"error":error,"message":"keep-secret"})))
            }),
        );
        let task = tokio::spawn(async move { axum::serve(listener, router).await });
        let result = cached_token(&store, "xrun-client", &http()?, &endpoint).await;
        task.abort();
        if error == "invalid_grant" {
            assert!(result?.is_none());
            assert!(store.load()?.is_none());
        } else {
            assert!(!result.err().unwrap().to_string().contains("keep-secret"));
            assert_eq!(
                store.load()?.unwrap().refresh_token.as_deref(),
                Some("keep-secret")
            );
        }
    }
    Ok(())
}
#[tokio::test]
async fn revocation_failure_preserves_the_login_and_success_revokes_both_tokens_before_deleting()
-> Result<()> {
    for status in [StatusCode::SERVICE_UNAVAILABLE, StatusCode::OK] {
        let store = MemoryVault::default();
        store.save(&expired())?;
        let tokens = Arc::new(Mutex::new(Vec::new()));
        let observed = tokens.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/revoke", listener.local_addr()?);
        let router = Router::new().route(
            "/revoke",
            axum::routing::post(
                move |axum::Form(form): axum::Form<HashMap<String, String>>| {
                    observed.lock().unwrap().push(form["token"].clone());
                    async move { status }
                },
            ),
        );
        let task = tokio::spawn(async move { axum::serve(listener, router).await });
        let result = revoke_saved(&store, &http()?, &endpoint).await;
        task.abort();
        if status.is_success() {
            result?;
            assert!(store.load()?.is_none());
            assert_eq!(*tokens.lock().unwrap(), ["keep-secret", "expired"]);
        } else {
            assert!(crate::error::is(
                &result.unwrap_err(),
                ErrorCode::OAuthRevocationFailed
            ));
            assert!(store.load()?.is_some());
        }
    }
    Ok(())
}
#[tokio::test]
async fn malformed_store_and_token_responses_never_expose_secret_contents() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let secret = "private-token-and-route";
    let error = vault::decode(&format!("{{\"access_token\":\"{secret}"))
        .err()
        .unwrap();
    assert!(!format!("{error:#}").contains(secret));
    let path = state::Deployment::path(directory.path(), "relay")?;
    std::fs::write(&path, format!("route = \"{secret}\n"))?;
    let error = state::Deployment::load(directory.path(), "relay")
        .err()
        .context("expected malformed record")?;
    assert!(!format!("{error:#}").contains(secret));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/token", listener.local_addr()?);
    let router = Router::new().route(
        "/token",
        axum::routing::post(move || async move {
            Json(json!({"access_token":secret, "expires_in":secret, "token_type":"Bearer"}))
        }),
    );
    let task = tokio::spawn(async move { axum::serve(listener, router).await });
    let result = exchange(&http()?, &endpoint, &[("grant_type", "refresh_token")]).await;
    task.abort();
    let error = result.err().context("invalid response")?;
    assert!(!format!("{error:#}").contains(secret));
    Ok(())
}
