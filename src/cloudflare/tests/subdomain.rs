use super::*;

#[tokio::test]
async fn uninitialized_accounts_register_before_worker_requests_and_reuse_the_subdomain()
-> Result<()> {
    let server = server().await?;
    {
        let mut state = server.state.lock().unwrap();
        state.new_subdomain = true;
        state.subdomain_lookup_error = Some((StatusCode::FORBIDDEN, vec![10063]));
    }
    let directory = tempfile::tempdir()?;
    let api =
        || Api::new("test-api-token".into()).map(|api| api.test_origin(server.origin.clone()));
    let first = deploy_with(
        api()?,
        directory.path(),
        "xrun-relay",
        ACCOUNT,
        &bundle()?,
        None,
    )
    .await?;
    assert!(first.link == "https://xrun-relay.xrun-0123456789ab.workers.dev");
    // Existing subdomains must not be registered again on a deployment retry.
    server.state.lock().unwrap().subdomain_registration_error =
        Some((StatusCode::FORBIDDEN, vec![10000]));
    let second = deploy_with(
        api()?,
        directory.path(),
        "xrun-relay",
        ACCOUNT,
        &bundle()?,
        None,
    )
    .await?;
    assert_eq!(first.link, second.link);
    assert!(server.state.lock().unwrap().enabled);
    assert_eq!(server.state.lock().unwrap().uploads.len(), 2);
    Ok(())
}

#[tokio::test]
async fn subdomain_authentication_failures_never_register_or_upload_a_worker() -> Result<()> {
    for (status, codes) in [
        (StatusCode::FORBIDDEN, vec![10000]),
        (StatusCode::FORBIDDEN, vec![10063, 10000]),
        (StatusCode::UNAUTHORIZED, vec![10063]),
    ] {
        let server = server().await?;
        server.state.lock().unwrap().subdomain_lookup_error = Some((status, codes));
        let directory = tempfile::tempdir()?;
        let failure = deploy_with(
            Api::new("test-api-token".into())?.test_origin(server.origin.clone()),
            directory.path(),
            "xrun-relay",
            ACCOUNT,
            &bundle()?,
            None,
        )
        .await
        .err()
        .unwrap();
        assert!(error::is(&failure, ErrorCode::Unauthenticated));
        assert!(!failure.to_string().contains("test-api-token"));
        let state = server.state.lock().unwrap();
        assert!(state.registered_subdomain.is_none());
        assert!(state.uploads.is_empty());
        assert!(Deployment::load(directory.path(), "xrun-relay")?.is_none());
    }
    Ok(())
}

#[tokio::test]
async fn registration_that_requires_dashboard_setup_has_an_actionable_error() -> Result<()> {
    let server = server().await?;
    {
        let mut state = server.state.lock().unwrap();
        state.new_subdomain = true;
        state.subdomain_registration_error = Some((StatusCode::FORBIDDEN, vec![10063]));
    }
    let directory = tempfile::tempdir()?;
    let failure = deploy_with(
        Api::new("test-api-token".into())?.test_origin(server.origin.clone()),
        directory.path(),
        "xrun-relay",
        ACCOUNT,
        &bundle()?,
        None,
    )
    .await
    .err()
    .unwrap();
    assert!(error::is(&failure, ErrorCode::InvalidConfig));
    assert!(failure.to_string().contains("Workers & Pages"));
    assert!(!failure.to_string().contains("test-api-token"));
    let state = server.state.lock().unwrap();
    assert!(state.registered_subdomain.is_none());
    assert!(state.uploads.is_empty());
    assert!(Deployment::load(directory.path(), "xrun-relay")?.is_none());
    Ok(())
}
