use super::*;
use anyhow::Result;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
    routing::get,
};
use tower::ServiceExt;

#[tokio::test]
async fn occupied_callback_ports_are_skipped_and_exhaustion_is_reported() -> Result<()> {
    let first = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let second = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let first_uri = format!("http://{}/oauth/callback", first.local_addr()?);
    let second_uri = format!("http://{}/oauth/callback", second.local_addr()?);
    let second_address = second.local_addr()?;
    let error = listen(&[&first_uri, &second_uri]).await.unwrap_err();
    assert!(crate::error::is(&error, ErrorCode::InvalidPort));
    assert!(error.to_string().contains("all occupied"));
    drop(second);
    let selected = listen(&[&first_uri, &second_uri]).await?;
    assert_eq!(selected.local_addr()?, second_address);
    assert!(selected.local_addr()?.ip().is_loopback());
    drop(selected);
    let _released = tokio::net::TcpListener::bind(second_address).await?;
    Ok(())
}

#[tokio::test]
async fn callback_listeners_reject_unregistered_hosts_and_paths() -> Result<()> {
    for uri in [
        "https://127.0.0.1:8976/oauth/callback",
        "http://localhost:8976/oauth/callback",
        "http://0.0.0.0:8976/oauth/callback",
        "http://127.0.0.1:0/oauth/callback",
        "http://127.0.0.1/oauth/callback",
        "http://127.0.0.1:8976/other",
        "http://user@127.0.0.1:8976/oauth/callback",
        "http://127.0.0.1:8976/oauth/callback?code=test",
        "http://127.0.0.1:8976/oauth/callback#fragment",
    ] {
        let error = listen(&[uri]).await.unwrap_err();
        assert!(crate::error::is(&error, ErrorCode::InvalidConfig), "{uri}");
    }
    Ok(())
}

#[tokio::test]
async fn provider_failures_keep_their_identity_details_and_single_use_callback() -> Result<()> {
    for (error, expected) in [
        ("invalid_scope", ErrorCode::OAuthInvalidScope),
        ("access_denied", ErrorCode::Unauthenticated),
        ("invalid_client", ErrorCode::InvalidConfig),
        ("unauthorized_client", ErrorCode::InvalidConfig),
        ("invalid_request", ErrorCode::InvalidConfig),
        ("server_error", ErrorCode::Unauthenticated),
        ("temporarily_unavailable", ErrorCode::Unauthenticated),
    ] {
        let (sender, receiver) = oneshot::channel();
        let router = Router::new()
            .route("/oauth/callback", get(receive))
            .with_state(Arc::new(Callback {
                state: "owned-test-state".into(),
                sender: Mutex::new(Some(sender)),
            }));
        let query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("state", "owned-test-state"),
                ("error", error),
                (
                    "error_description",
                    "The OAuth client is not allowed to request scope 'workers-scripts.write'.",
                ),
                ("code", "unused-test-code"),
            ])
            .finish();
        let request = || {
            Request::builder()
                .uri(format!("/oauth/callback?{query}"))
                .body(Body::empty())
        };
        let response = router.clone().oneshot(request()?).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = String::from_utf8(to_bytes(response.into_body(), 16384).await?.to_vec())?;
        let failure = receiver.await?.unwrap_err();
        assert_eq!(failure.code, expected);
        assert!(failure.message.contains(error));
        assert!(failure.message.contains("workers-scripts.write"));
        assert_eq!(
            failure.message.contains("was declined"),
            error == "access_denied"
        );
        assert!(!body.contains("owned-test-state"));
        assert!(!body.contains("unused-test-code"));
        assert!(body.contains("history.replaceState"));
        let wire = crate::error::wire(&anyhow::Error::new(failure));
        assert_eq!(wire.0, expected.as_str());
        assert!(wire.1.contains(error));
        assert_eq!(router.oneshot(request()?).await?.status(), StatusCode::GONE);
    }
    Ok(())
}

#[tokio::test]
async fn invalid_states_do_not_consume_a_provider_failure() -> Result<()> {
    let (sender, mut receiver) = oneshot::channel();
    let router = Router::new()
        .route("/oauth/callback", get(receive))
        .with_state(Arc::new(Callback {
            state: "owned-test-state".into(),
            sender: Mutex::new(Some(sender)),
        }));
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/oauth/callback?state=wrong&error=invalid_scope")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(matches!(
        receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    let response = router
        .oneshot(
            Request::builder()
                .uri("/oauth/callback?state=owned-test-state&error=invalid_scope")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        receiver.await?.unwrap_err().code,
        ErrorCode::OAuthInvalidScope
    );
    Ok(())
}

#[tokio::test]
async fn callback_pages_escape_provider_descriptions_and_redact_query_secrets() -> Result<()> {
    let (sender, receiver) = oneshot::channel();
    let router = Router::new()
        .route("/oauth/callback", get(receive))
        .with_state(Arc::new(Callback {
            state: "owned-test-state".into(),
            sender: Mutex::new(Some(sender)),
        }));
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs([
            ("state", "owned-test-state"),
            ("error", "invalid_scope"),
            ("code", "unused-test-code"),
            ("access_token", "test-only-access-token"),
            (
                "error_description",
                "<b>permission</b> & owned-test-state unused-test-code test-only-access-token\r\n",
            ),
        ])
        .finish();
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/oauth/callback?{query}"))
                .body(Body::empty())?,
        )
        .await?;
    let body = String::from_utf8(to_bytes(response.into_body(), 16384).await?.to_vec())?;
    assert!(body.contains("&lt;b&gt;permission&lt;/b&gt; &amp;"));
    assert!(!body.contains("<b>"));
    let detail = receiver.await?.unwrap_err().message;
    for secret in [
        "owned-test-state",
        "unused-test-code",
        "test-only-access-token",
    ] {
        assert!(!body.contains(secret));
        assert!(!detail.contains(secret));
    }
    assert!(!detail.chars().any(char::is_control));
    assert!(detail.contains("[redacted]"));
    Ok(())
}

#[test]
fn provider_details_are_bounded_and_malformed_error_names_are_not_echoed() {
    let query = HashMap::from([
        ("error".into(), "invalid_scope".into()),
        ("error_description".into(), "x".repeat(8192)),
    ]);
    assert_eq!(failure(&query).message.chars().count(), 2048);
    for name in ["", "<b>", "error\u{1b}[31m", &"e".repeat(256)] {
        let query = HashMap::from([("error".into(), name.into())]);
        let error = failure(&query);
        assert!(error.message.contains("invalid_error_response"));
        assert!(!error.message.contains("was declined"));
    }
}
