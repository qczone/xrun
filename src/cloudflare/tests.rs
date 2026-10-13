use super::{
    api::Api,
    bundle::Bundle,
    deploy::deploy_with,
    manage::{link_in, remove_with},
    state::{Deployment, DeploymentStatus},
};
use crate::{
    error::{self, ErrorCode},
    protocol::{ProtocolRange, VERSION, sha256},
};
use anyhow::Result;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Method, Request, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";

mod subdomain;

fn bundle() -> Result<Bundle> {
    let source =
        "export class XrunRelay {}\nexport default {fetch() {return new Response('test')}};";
    Bundle::parse(
        &json!({"schema":1,"version":VERSION,"sha256":sha256(source.as_bytes()),
        "compatibility_date":"2026-10-03","protocol":ProtocolRange::CURRENT})
        .to_string(),
        source,
    )
}

#[derive(Default)]
struct Mock {
    accounts: Vec<Value>,
    metadata: Option<Value>,
    uploads: Vec<Value>,
    enabled: bool,
    fail_enable: bool,
    new_subdomain: bool,
    subdomain_lookup_error: Option<(StatusCode, Vec<u32>)>,
    subdomain_registration_error: Option<(StatusCode, Vec<u32>)>,
    registered_subdomain: Option<String>,
    forbidden: bool,
    migration: Option<String>,
    fail_delete: bool,
    deletions: usize,
}

struct Server {
    origin: String,
    state: Arc<Mutex<Mock>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn server() -> Result<Server> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let state = Arc::new(Mutex::new(Mock::default()));
    let router = Router::new().fallback(mock).with_state(state.clone());
    let task = tokio::spawn(async move { axum::serve(listener, router).await });
    Ok(Server {
        origin,
        state,
        task,
    })
}

fn reply(value: Value) -> Response {
    Json(json!({"success":true,"result":value})).into_response()
}
fn missing() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"success":false,"errors":[{"code":10007,"message":"not found"}]})),
    )
        .into_response()
}

fn api_failure(status: StatusCode, codes: &[u32]) -> Response {
    let errors: Vec<_> = codes
        .iter()
        .map(|code| json!({"code":code,"message":"test-api-token request failed"}))
        .collect();
    (status, Json(json!({"success":false,"errors":errors}))).into_response()
}

async fn mock(State(state): State<Arc<Mutex<Mock>>>, request: Request<Body>) -> Response {
    assert_eq!(request.headers()["authorization"], "Bearer test-api-token");
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    let body = to_bytes(request.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap();
    let mut state = state.lock().unwrap();
    if state.forbidden {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"success":false,"errors":[
            {"code":10000,"message":"test-api-token is denied"}]})),
        )
            .into_response();
    }
    if state.new_subdomain
        && (path.ends_with("/workers/scripts")
            || (method == Method::PUT && path.ends_with("/workers/scripts/xrun-relay")))
    {
        return api_failure(StatusCode::FORBIDDEN, &[10063]);
    }
    if path.ends_with("/workers/scripts") {
        return match &state.metadata {
            Some(metadata) => reply(
                json!([{"id":"xrun-relay","tags":metadata["tags"],"migration_tag":state.migration.as_deref().unwrap_or("v1") }]),
            ),
            None => reply(json!([])),
        };
    }
    if path == "/accounts" {
        return reply(json!(state.accounts));
    }
    if path.ends_with("/workers/subdomain") {
        if method == Method::PUT {
            if let Some((status, codes)) = &state.subdomain_registration_error {
                return api_failure(*status, codes);
            }
            let body: Value = serde_json::from_slice(&body).unwrap();
            state.registered_subdomain = Some(body["subdomain"].as_str().unwrap().into());
            state.new_subdomain = false;
            state.subdomain_lookup_error = None;
            return reply(body);
        }
        if let Some((status, codes)) = &state.subdomain_lookup_error {
            return api_failure(*status, codes);
        }
        return if state.new_subdomain {
            missing()
        } else {
            reply(json!({"subdomain":state.registered_subdomain.as_deref().unwrap_or("example")}))
        };
    }
    if method == Method::POST && path.ends_with("/subdomain") {
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body, json!({"enabled":true,"previews_enabled":false}));
        if state.fail_enable {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"success":false,"errors":[
                {"code":10001,"message":"try later"}]})),
            )
                .into_response();
        }
        state.enabled = true;
        return reply(body);
    }
    if method == Method::PUT && path.ends_with("/xrun-relay") {
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("application/javascript+module\r\n\r\nexport"));
        let metadata: Value = serde_json::from_str(
            text.split("\r\n\r\n")
                .nth(1)
                .unwrap()
                .split("\r\n--")
                .next()
                .unwrap(),
        )
        .unwrap();
        if let Some(tag) = metadata["migrations"]["new_tag"].as_str() {
            state.migration = Some(tag.to_owned());
        }
        state.uploads.push(metadata.clone());
        state.metadata = Some(metadata);
        return reply(json!({"id":"xrun-relay"}));
    }
    if method == Method::DELETE && path.ends_with("/xrun-relay") {
        state.deletions += 1;
        if state.fail_delete {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"success":false,"errors":[{"code":10001,"message":"try later"}]})),
            )
                .into_response();
        }
        state.metadata = None;
        state.enabled = false;
        return reply(Value::Null);
    }
    panic!("unexpected Cloudflare request: {method} {path}");
}

#[tokio::test]
async fn new_deployment_and_updates_keep_the_origin_and_durable_objects() -> Result<()> {
    let server = server().await?;
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
    assert_eq!(first.link, "https://xrun-relay.example.workers.dev");
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
    let state = server.state.lock().unwrap();
    assert!(state.enabled);
    assert_eq!(state.uploads.len(), 2);
    assert_eq!(
        state.uploads[0]["migrations"],
        json!({"new_tag":"v1","new_sqlite_classes":["XrunRelay","XrunData"]})
    );
    assert!(state.uploads[1].get("migrations").is_none());
    assert_eq!(state.uploads[0]["bindings"], state.uploads[1]["bindings"]);
    assert!(
        state.uploads[0]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|binding| binding["name"] != "RELAY_ROUTE")
    );
    assert_eq!(
        state.uploads[0]["bindings"][2]["json"],
        ProtocolRange::CURRENT.min
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(Deployment::path(directory.path(), "xrun-relay")?)?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    Ok(())
}

#[tokio::test]
async fn failed_enable_can_be_retried_without_recreating_the_relay() -> Result<()> {
    let server = server().await?;
    server.state.lock().unwrap().fail_enable = true;
    let directory = tempfile::tempdir()?;
    let api =
        || Api::new("test-api-token".into()).map(|api| api.test_origin(server.origin.clone()));
    assert!(
        deploy_with(
            api()?,
            directory.path(),
            "xrun-relay",
            ACCOUNT,
            &bundle()?,
            None
        )
        .await
        .is_err()
    );
    let saved = Deployment::load(directory.path(), "xrun-relay")?.unwrap();
    assert!(saved.url.is_none());
    server.state.lock().unwrap().fail_enable = false;
    let result = deploy_with(
        api()?,
        directory.path(),
        "xrun-relay",
        ACCOUNT,
        &bundle()?,
        None,
    )
    .await?;
    assert_eq!(result.link, "https://xrun-relay.example.workers.dev");
    let state = server.state.lock().unwrap();
    assert!(state.uploads[1].get("migrations").is_none());
    Ok(())
}

#[tokio::test]
async fn existing_unmanaged_workers_are_not_overwritten() -> Result<()> {
    let server = server().await?;
    server.state.lock().unwrap().metadata = Some(json!({"tags":["another-app"]}));
    let directory = tempfile::tempdir()?;
    let error = deploy_with(
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
    assert!(error::is(&error, ErrorCode::AlreadyExists));
    assert!(server.state.lock().unwrap().uploads.is_empty());
    assert!(Deployment::load(directory.path(), "xrun-relay")?.is_none());
    Ok(())
}

#[tokio::test]
async fn new_accounts_get_a_workers_dev_subdomain() -> Result<()> {
    let server = server().await?;
    server.state.lock().unwrap().new_subdomain = true;
    let directory = tempfile::tempdir()?;
    let result = deploy_with(
        Api::new("test-api-token".into())?.test_origin(server.origin.clone()),
        directory.path(),
        "xrun-relay",
        ACCOUNT,
        &bundle()?,
        None,
    )
    .await?;
    assert!(result.link == "https://xrun-relay.xrun-0123456789ab.workers.dev");
    assert_eq!(
        server.state.lock().unwrap().registered_subdomain.as_deref(),
        Some("xrun-0123456789ab")
    );
    Ok(())
}

#[tokio::test]
async fn denied_api_calls_are_not_missing_workers_and_tokens_are_redacted() -> Result<()> {
    let server = server().await?;
    server.state.lock().unwrap().forbidden = true;
    let directory = tempfile::tempdir()?;
    let error = deploy_with(
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
    assert!(error::is(&error, ErrorCode::Unauthenticated));
    assert!(!error.to_string().contains("test-api-token"));
    assert!(server.state.lock().unwrap().uploads.is_empty());
    assert!(Deployment::load(directory.path(), "xrun-relay")?.is_none());
    Ok(())
}

#[test]
fn mismatched_worker_versions_and_changed_bundles_are_rejected() -> Result<()> {
    let bundle = bundle()?;
    let manifest = json!({"schema":1,"version":VERSION,"sha256":bundle.manifest.sha256,
        "compatibility_date":"2026-10-03","protocol":ProtocolRange::CURRENT});
    assert!(error::is(
        &Bundle::parse(&manifest.to_string(), "changed")
            .err()
            .unwrap(),
        ErrorCode::ChecksumMismatch
    ));
    let mut future = manifest;
    future["version"] = json!("future");
    assert!(error::is(
        &Bundle::parse(&future.to_string(), &bundle.source)
            .err()
            .unwrap(),
        ErrorCode::VersionMismatch
    ));
    assert!(Deployment::path(std::path::Path::new("."), "../other").is_err());
    Ok(())
}

#[tokio::test]
async fn saved_links_are_offline_and_removal_never_deletes_an_unowned_worker() -> Result<()> {
    let server = server().await?;
    let directory = tempfile::tempdir()?;
    let api =
        || Api::new("test-api-token".into()).map(|api| api.test_origin(server.origin.clone()));
    assert!(link_in(directory.path(), "xrun-relay").is_err());
    assert!(
        remove_with(api()?, directory.path(), "xrun-relay")
            .await
            .is_err()
    );
    let relay = deploy_with(
        api()?,
        directory.path(),
        "xrun-relay",
        ACCOUNT,
        &bundle()?,
        None,
    )
    .await?;
    assert_eq!(link_in(directory.path(), "xrun-relay")?.link, relay.link);
    server.state.lock().unwrap().metadata = Some(json!({"tags":["another-app"]}));
    let error = remove_with(api()?, directory.path(), "xrun-relay")
        .await
        .err()
        .unwrap();
    assert!(error::is(&error, ErrorCode::AlreadyExists));
    let remote = server.state.lock().unwrap();
    assert_eq!(remote.uploads.len(), 1);
    assert_eq!(remote.deletions, 0);
    assert!(
        Deployment::load(directory.path(), "xrun-relay")?
            .unwrap()
            .status
            == DeploymentStatus::Active
    );
    Ok(())
}

#[tokio::test]
async fn interrupted_removal_retries_without_repeating_the_namespace_deletion() -> Result<()> {
    let server = server().await?;
    let directory = tempfile::tempdir()?;
    let api =
        || Api::new("test-api-token".into()).map(|api| api.test_origin(server.origin.clone()));
    deploy_with(
        api()?,
        directory.path(),
        "xrun-relay",
        ACCOUNT,
        &bundle()?,
        None,
    )
    .await?;
    server.state.lock().unwrap().fail_delete = true;
    assert!(
        remove_with(api()?, directory.path(), "xrun-relay")
            .await
            .is_err()
    );
    assert!(
        Deployment::load(directory.path(), "xrun-relay")?
            .unwrap()
            .status
            == DeploymentStatus::Removing
    );
    assert!(link_in(directory.path(), "xrun-relay").is_err());
    assert!(
        deploy_with(
            api()?,
            directory.path(),
            "xrun-relay",
            ACCOUNT,
            &bundle()?,
            None
        )
        .await
        .is_err()
    );
    {
        let mut remote = server.state.lock().unwrap();
        assert_eq!(remote.uploads.len(), 2);
        assert_eq!(
            remote.uploads[1]["migrations"],
            json!({"old_tag":"v1","new_tag":"removed","deleted_classes":["XrunRelay","XrunData"]})
        );
        assert_eq!(remote.uploads[1]["bindings"], json!([]));
        remote.fail_delete = false;
    }
    let removed = remove_with(api()?, directory.path(), "xrun-relay").await?;
    assert!(removed.removed && !removed.data_retained);
    assert!(
        Deployment::load(directory.path(), "xrun-relay")?
            .unwrap()
            .status
            == DeploymentStatus::Removed
    );
    assert!(
        deploy_with(
            api()?,
            directory.path(),
            "xrun-relay",
            ACCOUNT,
            &bundle()?,
            None
        )
        .await
        .is_err()
    );
    {
        let mut remote = server.state.lock().unwrap();
        assert_eq!(remote.uploads.len(), 2);
        assert_eq!(remote.deletions, 2);
        remote.metadata = Some(json!({"tags":["replacement-app"]}));
    }
    remove_with(api()?, directory.path(), "xrun-relay").await?;
    assert_eq!(server.state.lock().unwrap().deletions, 2);
    Ok(())
}

#[tokio::test]
async fn desktop_accounts_are_listed_without_terminal_selection_and_validate_ids() -> Result<()> {
    let server = server().await?;
    let api = Api::new("test-api-token".into())?.test_origin(server.origin.clone());
    server.state.lock().unwrap().accounts = vec![
        json!({"id":ACCOUNT,"name":"Personal"}),
        json!({"id":"a".repeat(32),"name":"Team"}),
    ];
    let accounts = super::deploy::account_list(&api).await?;
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[1].name, "Team");
    server.state.lock().unwrap().accounts.clear();
    assert!(error::is(
        &super::deploy::account_list(&api).await.err().unwrap(),
        ErrorCode::Unauthenticated
    ));
    server.state.lock().unwrap().accounts = vec![json!({"id":"not-an-account","name":"Invalid"})];
    assert!(error::is(
        &super::deploy::account_list(&api).await.err().unwrap(),
        ErrorCode::InvalidConfig
    ));
    Ok(())
}

#[tokio::test]
async fn region_hints_persist_across_current_deployment_updates() -> Result<()> {
    use super::Region;
    let server = server().await?;
    let directory = tempfile::tempdir()?;
    let api =
        || Api::new("test-api-token".into()).map(|api| api.test_origin(server.origin.clone()));
    let first = deploy_with(
        api()?,
        directory.path(),
        "xrun-relay",
        ACCOUNT,
        &bundle()?,
        Some(Region::ApacNe),
    )
    .await?;
    assert_eq!(first.region, Region::ApacNe);
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
    assert_eq!(second.region, Region::ApacNe);
    {
        let remote = server.state.lock().unwrap();
        assert!(remote.uploads[1].get("migrations").is_none());
        assert!(
            remote.uploads[1]["bindings"].as_array().unwrap().contains(
                &json!({"type":"plain_text","name":"XRUN_RELAY_REGION","text":"apac-ne"})
            )
        );
    }
    let automatic = deploy_with(
        api()?,
        directory.path(),
        "xrun-relay",
        ACCOUNT,
        &bundle()?,
        Some(Region::Auto),
    )
    .await?;
    assert_eq!(automatic.region, Region::Auto);
    assert!(
        !server.state.lock().unwrap().uploads[2]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|binding| binding["name"] == "XRUN_RELAY_REGION")
    );
    assert!(serde_json::from_str::<Region>("\"arbitrary\"").is_err());
    let path = Deployment::path(directory.path(), "xrun-relay")?;
    let saved = std::fs::read(&path)?;
    server.state.lock().unwrap().migration = Some("unsupported".into());
    let error = deploy_with(
        api()?,
        directory.path(),
        "xrun-relay",
        ACCOUNT,
        &bundle()?,
        None,
    )
    .await
    .err()
    .expect("unsupported Worker format must be rejected");
    assert!(crate::error::is(&error, ErrorCode::VersionMismatch));
    assert_eq!(server.state.lock().unwrap().uploads.len(), 3);
    assert_eq!(std::fs::read(path)?, saved);
    Ok(())
}
