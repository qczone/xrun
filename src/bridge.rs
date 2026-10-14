//! Private backend of the desktop App, run as `xrun _app-bridge` from the bundled helper.
//! The App links no core code. Each line is one JSON object: a request
//! `{"id","method","params"}` is answered by `{"id","result"}` or `{"id","error"}`, after
//! any `{"id","event","data"}` progress for it. Service registration belongs to the App
//! bundle, so the bridge asks for it with `{"call","method"}` and awaits `{"reply",...}`.
//! The process exits when the App closes its input.
use crate::{
    client::{
        self,
        relay::{
            self, DeployedRelay, DeploymentReporter, DeploymentStage as Stage, ProgressDetails,
            ProgressStatus,
        },
        services,
    },
    error::{CodedError, ErrorCode},
    protocol::Data,
};
use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::Future,
    io::{BufRead, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use zeroize::{Zeroize, Zeroizing};

/// Hidden subcommand. Device names begin with a lowercase letter, so it never shadows one.
pub(crate) const COMMAND: &str = "_app-bridge";

/// Serve the App over standard input and output until the App closes the channel.
pub(crate) async fn run() -> Result<()> {
    let output = protocol_output()?;
    serve(std::io::BufReader::new(std::io::stdin()), output).await
}

/// Reserve the inherited stdout for protocol lines; other writes go to stderr.
#[cfg(unix)]
fn protocol_output() -> Result<std::fs::File> {
    use std::os::fd::AsFd;
    let channel = duplicate_output(std::io::stdout().as_fd())?;
    if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(channel)
}

/// Owned descriptor with close-on-exec, so descendants cannot keep the App's
/// response pipe open after this process exits. Ownership also covers dup2 errors.
#[cfg(unix)]
fn duplicate_output(output: std::os::fd::BorrowedFd<'_>) -> std::io::Result<std::fs::File> {
    Ok(std::fs::File::from(output.try_clone_to_owned()?))
}
/// Reserve the inherited stdout for protocol lines; std resolves the handle on each write.
#[cfg(windows)]
fn protocol_output() -> Result<std::fs::File> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::{
        Foundation::{HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation},
        System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle},
    };
    let channel = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if channel.is_null()
        || channel == INVALID_HANDLE_VALUE
        || unsafe { SetHandleInformation(channel, HANDLE_FLAG_INHERIT, 0) } == 0
        || unsafe { SetStdHandle(STD_OUTPUT_HANDLE, GetStdHandle(STD_ERROR_HANDLE)) } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { std::fs::File::from_raw_handle(channel) })
}

/// Serve one App connection. Requests run concurrently; input end-of-file ends serving.
pub(crate) async fn serve(
    input: impl BufRead + Send + 'static,
    output: impl Write + Send + 'static,
) -> Result<()> {
    Bridge::start(output).listen(input).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    id: Option<u64>,
    method: Option<String>,
    #[serde(default)]
    params: Value,
    reply: Option<u64>,
    #[serde(default)]
    result: Value,
    error: Option<Failure>,
}

/// A coded failure in either direction; the App branches on codes, never on prose.
#[derive(Deserialize, Serialize)]
struct Failure {
    code: String,
    message: String,
}

impl Failure {
    fn from_error(error: &anyhow::Error) -> Self {
        let (code, message) = crate::error::wire(error);
        Self { code, message }
    }
}

struct Bridge {
    sender: mpsc::UnboundedSender<Value>,
    calls: Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>,
    next_call: AtomicU64,
    /// Ends account selection while its browser authorization is pending.
    cloudflare_cancel: Mutex<Option<oneshot::Sender<()>>>,
    /// Authorization address the App can reopen while it remains pending.
    cloudflare_authorization: Mutex<Option<String>>,
}

/// Decode named parameters; unknown fields are rejected so App and helper stay in step.
macro_rules! params {
    ($params:expr, $($name:ident: $type:ty),+ $(,)?) => {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Params {
            $($name: $type),+
        }
        let Params { $($name),+ } = decode::<Params>($params)?;
    };
}

fn decode<T: DeserializeOwned>(params: Value) -> Result<T> {
    let params = if params.is_null() { json!({}) } else { params };
    serde_json::from_value(params).map_err(|error| {
        ErrorCode::InvalidRequest
            .error(format!("invalid parameters: {error}"))
            .into()
    })
}

fn encode(value: impl Serialize) -> Result<Value> {
    Ok(serde_json::to_value(value)?)
}

/// Run file and database work off the runtime threads that serve other requests.
async fn blocking<T: Serialize + Send + 'static>(
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<Value> {
    encode(tokio::task::spawn_blocking(operation).await??)
}

impl Bridge {
    /// Announce readiness; one writer thread keeps each message on a single line.
    fn start(mut output: impl Write + Send + 'static) -> Arc<Self> {
        let (sender, mut outgoing) = mpsc::unbounded_channel::<Value>();
        std::thread::spawn(move || -> std::io::Result<()> {
            while let Some(message) = outgoing.blocking_recv() {
                let mut line = serde_json::to_vec(&message)?;
                line.push(b'\n');
                let written = output.write_all(&line).and_then(|()| output.flush());
                line.zeroize();
                written?;
            }
            Ok(())
        });
        let bridge = Arc::new(Self {
            sender,
            calls: Mutex::default(),
            next_call: AtomicU64::new(1),
            cloudflare_cancel: Mutex::default(),
            cloudflare_authorization: Mutex::default(),
        });
        bridge.send(json!({"ready": {"version": crate::protocol::VERSION}}));
        bridge
    }

    async fn listen(self: &Arc<Self>, input: impl BufRead + Send + 'static) -> Result<()> {
        let (lines, mut incoming) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            for line in input.lines() {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });
        while let Some(line) = incoming.recv().await {
            let mut line = line?;
            self.receive(&line);
            // Authority export and restore requests carry passwords.
            line.zeroize();
        }
        Ok(())
    }

    fn send(&self, message: Value) {
        let _ = self.sender.send(message);
    }

    fn notify(&self, request: u64, event: &str, data: impl Serialize) {
        if let Ok(data) = serde_json::to_value(data) {
            self.send(json!({"id": request, "event": event, "data": data}));
        }
    }

    fn receive(self: &Arc<Self>, line: &str) {
        match serde_json::from_str::<Message>(line) {
            Ok(Message {
                id: Some(id),
                method: Some(method),
                params,
                reply: None,
                ..
            }) => {
                let bridge = self.clone();
                tokio::spawn(async move {
                    let response = match Box::pin(bridge.dispatch(id, &method, params)).await {
                        Ok(result) => json!({"id": id, "result": result}),
                        Err(error) => json!({"id": id, "error": Failure::from_error(&error)}),
                    };
                    bridge.send(response);
                });
            }
            Ok(Message {
                id: None,
                method: None,
                reply: Some(call),
                result,
                error,
                ..
            }) => {
                if let Some(waiter) = self.calls.lock().unwrap().remove(&call) {
                    let _ = waiter.send(match error {
                        Some(Failure { code, message }) => {
                            Err(CodedError::from_wire(code, message).into())
                        }
                        None => Ok(result),
                    });
                }
            }
            _ => {
                // Answer a malformed request instead of leaving the App waiting.
                let id = serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|message| message.get("id")?.as_u64());
                let failure = Failure {
                    code: ErrorCode::InvalidRequest.as_str().into(),
                    message: "malformed desktop bridge message".into(),
                };
                match id {
                    Some(id) => self.send(json!({"id": id, "error": failure})),
                    None => tracing::warn!("{}", failure.message),
                }
            }
        }
    }

    /// Ask the App for an operation only its bundle can perform.
    async fn call(&self, method: &str) -> Result<Value> {
        let call = self.next_call.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.calls.lock().unwrap().insert(call, sender);
        self.send(json!({"call": call, "method": method}));
        receiver.await.map_err(|_| {
            anyhow!(ErrorCode::ServiceUnavailable.error("the App closed the bridge"))
        })?
    }

    async fn dispatch(
        self: &Arc<Self>,
        request: u64,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        match method {
            // Liveness is independent of an operation waiting for a person or IO.
            "ping" => Ok(Value::Null),
            // Local status and settings; none of these contact a relay.
            "local_status" => blocking(client::local_status).await,
            "local_snapshot" => blocking(snapshot).await,
            "settings" => blocking(client::settings).await,
            "save_settings" => {
                params!(params, execution: client::ExecutionSettings);
                blocking(move || client::save_settings(execution)).await
            }
            "save_attachment_retention" => {
                params!(params, days: u16);
                blocking(move || client::save_attachment_retention(days)).await
            }
            "save_membership_lease" => {
                params!(params, minutes: u16);
                encode(client::save_membership_lease(minutes).await?)
            }
            "save_traffic_compression" => {
                params!(params, compression: crate::protocol::Compression);
                encode(client::save_traffic_compression(compression).await?)
            }
            "save_direct_connection" => {
                params!(params, address: Option<std::net::Ipv4Addr>);
                encode(client::save_direct_connection(address).await?)
            }
            "private_interfaces" => blocking(crate::lan::interfaces).await,
            "suggested_name" => encode(client::suggested_name()),
            "device_status" => encode(client::device_status().await?),
            "traffic" => {
                params!(params, query: crate::protocol::TrafficQuery);
                encode(client::traffic(query).await?)
            }
            // Local activity history.
            "job_output" => {
                params!(params, db_id: String, job: String, after: Option<u64>);
                blocking(move || client::history::output(&db_id, &job, after)).await
            }
            "activity_job" => {
                params!(params, db_id: String, id: String);
                blocking(move || client::history::job(&db_id, &id)).await
            }
            "activity_history" => {
                params!(params, before: Option<String>, filter: String);
                blocking(move || client::history::activity(before.as_deref(), &filter)).await
            }
            "activity_attachment" => {
                params!(params, id: String);
                blocking(move || client::history::attachment(&id)).await
            }
            "save_activity_attachment" => {
                params!(params, id: String, destination: PathBuf);
                blocking(move || client::history::save_attachment(&id, &destination)).await
            }
            // Membership.
            "connect" => {
                params!(params, link: String, name: Option<String>, allow_manager: bool);
                encode(client::connect(&link, name, allow_manager).await?)
            }
            "create_network" => {
                params!(params, link: String, name: Option<String>);
                encode(client::create_network(&link, name).await?)
            }
            "join" => {
                params!(params, link: String, name: Option<String>);
                encode(client::join(&link, name).await?)
            }
            "invite" => {
                params!(params, allow: bool, replace: Option<String>, full_link: bool);
                encode(if full_link {
                    client::invite_link(allow).await?
                } else {
                    client::invite_code(allow, replace.as_deref()).await?
                })
            }
            "cancel_invitation" => {
                params!(params, invitation_id: String);
                encode(client::cancel_invitation(&invitation_id).await?)
            }
            "code_link" => {
                params!(params, address: String, code: String);
                encode(client::code_link(&address, &code)?)
            }
            "revoke" => {
                params!(params, device: String);
                encode(client::revoke(&device).await?)
            }
            "set_permission" => {
                params!(params, device: String, allow: bool);
                encode(client::set_permission(&device, allow).await?)
            }
            "set_all_permissions" => {
                params!(params, allow: bool);
                encode(client::set_all_permissions(allow).await?)
            }
            "pause_access" => {
                params!(params, paused: bool);
                encode(client::pause_access(paused).await?)
            }
            // Encrypted authority archives.
            "export_authority" => {
                params!(params, path: PathBuf, password: Zeroizing<String>, device: Option<String>);
                blocking(move || match device {
                    Some(device) => {
                        client::network::migrate_authority(&device, &path, password.as_bytes())
                    }
                    None => client::network::backup_authority(&path, password.as_bytes()),
                })
                .await
            }
            "restore_authority" => {
                params!(params, path: PathBuf, password: Zeroizing<String>);
                let restored =
                    client::network::restore_authority(&path, password.as_bytes()).await?;
                blocking(services::initialize_daemon).await?;
                encode(restored)
            }
            "retire_authority" => encode(client::network::retire_authority().await?),
            // Daemon lifecycle. Supervisor registration stays with the App bundle.
            "daemon_running" => blocking(services::daemon_running).await,
            "daemon_state" => {
                params!(params, directory: Option<PathBuf>);
                blocking(move || {
                    let directory = directory.map_or_else(services::data_dir, Ok)?;
                    let state = services::daemon_state(&directory)?;
                    Ok(json!({"running": state.running, "generation": state.generation}))
                })
                .await
            }
            "initialize_daemon" => blocking(services::initialize_daemon).await,
            "daemon_restart_required" => blocking(services::daemon_restart_required).await,
            "daemon_restart_pending" => blocking(services::daemon_restart_pending).await,
            "stop_daemon_for_restart" => encode(services::stop_daemon_for_restart().await?),
            "finish_daemon_restart" => blocking(services::finish_daemon_restart).await,
            "stop_daemon" => encode(services::stop_daemon().await?),
            "daemon_installed" => blocking(services::daemon_installed).await,
            #[cfg(target_os = "macos")]
            "cli_daemon_installed" => blocking(services::cli_daemon_installed).await,
            "install_daemon" => {
                params!(params, executable: PathBuf);
                encode(services::install_daemon_with_executable(&executable).await?)
            }
            "uninstall_daemon" => encode(services::uninstall_daemon().await?),
            // Cloudflare relays, reporting progress events for the requesting operation.
            "cloudflare_accounts" => {
                params!(
                    params,
                    reauthorize: bool,
                    remember: Option<bool>,
                    progress_id: Option<String>,
                    finish_temporary: Option<bool>,
                );
                let remember = remember.unwrap_or(false);
                let finish = finish_temporary.unwrap_or(false);
                self.cloudflare_accounts(request, reauthorize, remember, progress_id, finish)
                    .await
            }
            "cancel_cloudflare_authorization" => self.cancel_cloudflare_authorization().await,
            "open_cloudflare_authorization" => {
                let url = self.cloudflare_authorization.lock().unwrap().clone();
                let Some(url) = url else {
                    bail!(ErrorCode::InvalidLink.error("authorization is no longer pending"));
                };
                encode(relay::open_cloudflare_authorization(&url)?)
            }
            "cloudflare_remembered_login" => blocking(relay::cloudflare_remembered_login).await,
            "logout_cloudflare" => {
                params!(params, progress_id: Option<String>);
                let reporter = self.reporter(request, progress_id, "logout_cloudflare");
                let result = reporter.scope(relay::logout_cloudflare()).await;
                reporter.report(
                    Stage::Complete,
                    if result.is_ok() {
                        ProgressStatus::Completed
                    } else {
                        ProgressStatus::Failed
                    },
                    0,
                    failure_details(&result),
                );
                encode(result?)
            }
            "create_cloudflare_network" => {
                Box::pin(self.create_cloudflare_network(request, params)).await
            }
            "cloudflare_relays" => blocking(relay::cloudflare_relays).await,
            // Authorized operations on remote devices.
            "remote_submit" => {
                params!(params, device: String, command: client::remote::CommandRequest);
                encode(client::remote::submit(&device, command).await?)
            }
            "remote_screenshot" => {
                params!(
                    params,
                    device: String,
                    path: PathBuf,
                    options: crate::protocol::ScreenshotOptions,
                );
                encode(client::remote::screenshot(&device, &path, options).await?)
            }
            "remote_recent" => {
                params!(params, device: String);
                blocking(move || client::remote::recent(&device)).await
            }
            "remote_query" => {
                params!(params, device: String, request_id: String);
                encode(client::remote::query(&device, &request_id).await?)
            }
            "remote_output" => {
                params!(params, device: String, db_id: String, job_id: String, after: u64);
                let output = client::remote::output(&device, &db_id, &job_id, after);
                encode(
                    tokio::time::timeout(Duration::from_secs(30), output)
                        .await
                        .map_err(|_| {
                            ErrorCode::ConnectTimeout.error("task output query timed out")
                        })??,
                )
            }
            "remote_cancel" => {
                params!(params, device: String, db_id: String, job_id: String);
                encode(client::remote::cancel(&device, &db_id, &job_id).await?)
            }
            "remote_file_receipts" => {
                params!(params, device: String);
                blocking(move || client::files::recent(&device)).await
            }
            "remote_file_upload" => {
                params!(
                    params,
                    device: String,
                    local: PathBuf,
                    options: client::files::UploadOptions,
                );
                encode(client::files::push(&device, &local, options).await?)
            }
            "remote_file_download" => {
                params!(
                    params,
                    device: String,
                    remote: String,
                    local: PathBuf,
                    cwd: Option<String>,
                );
                encode(client::files::pull(&device, &remote, Some(&local), cwd).await?)
            }
            "remote_file_resume" => {
                params!(params, device: String, request_id: String);
                encode(client::files::resume(&device, &request_id).await?)
            }
            "remote_file_cancel" => {
                params!(params, device: String, request_id: String);
                encode(client::files::cancel(&device, &request_id).await?)
            }
            _ => bail!(
                ErrorCode::InvalidRequest.error(format!("unknown desktop operation {method}"))
            ),
        }
    }

    fn reporter(
        self: &Arc<Self>,
        request: u64,
        id: Option<String>,
        operation: &str,
    ) -> DeploymentReporter {
        let bridge = self.clone();
        let reporter = DeploymentReporter::new(
            id.unwrap_or_else(|| format!("desktop-{}", crate::protocol::now_ms())),
            "cloudflare",
            operation,
            move |event| {
                // The browser has returned, so cancelling no longer applies to this handoff.
                if event.stage == Stage::AuthorizationReceived {
                    bridge.cloudflare_cancel.lock().unwrap().take();
                    bridge.cloudflare_authorization.lock().unwrap().take();
                }
                bridge.notify(request, "deployment_progress", event);
            },
        )
        .with_interactive(false);
        match services::data_dir() {
            Ok(directory) => reporter.with_log(directory.join("deployment.log")),
            Err(_) => reporter,
        }
    }

    fn prompt(self: &Arc<Self>, request: u64) -> impl Fn(&str) + Send + Sync + 'static {
        let bridge = self.clone();
        move |url: &str| {
            *bridge.cloudflare_authorization.lock().unwrap() = Some(url.to_owned());
            bridge.notify(request, "cloudflare_authorization", url);
        }
    }

    async fn cloudflare_accounts(
        self: &Arc<Self>,
        request: u64,
        reauthorize: bool,
        remember: bool,
        progress_id: Option<String>,
        finish_temporary: bool,
    ) -> Result<Value> {
        let (cancel, cancelled) = oneshot::channel();
        *self.cloudflare_cancel.lock().unwrap() = Some(cancel);
        let prompt = self.prompt(request);
        let reporter = self.reporter(request, progress_id, "cloudflare_accounts");
        let result: Result<Option<Vec<relay::Account>>> = reporter
            .scope(async {
                tokio::select! {
                    result = relay::cloudflare_accounts(&prompt, reauthorize, remember) => {
                        let accounts = result?;
                        if finish_temporary {
                            relay::end_cloudflare_authorization().await?;
                        }
                        Ok(Some(accounts))
                    },
                    Ok(()) = cancelled => Ok(None),
                }
            })
            .await;
        reporter.report(
            Stage::Complete,
            match &result {
                Ok(Some(_)) => ProgressStatus::Completed,
                Ok(None) => ProgressStatus::Cancelled,
                Err(_) => ProgressStatus::Failed,
            },
            0,
            failure_details(&result),
        );
        self.cloudflare_cancel.lock().unwrap().take();
        self.cloudflare_authorization.lock().unwrap().take();
        encode(result?)
    }

    async fn cancel_cloudflare_authorization(&self) -> Result<Value> {
        let account_selection = self.cloudflare_cancel.lock().unwrap().take();
        let selecting = account_selection.is_some();
        if let Some(cancel) = account_selection {
            let _ = cancel.send(());
        }
        let pending = relay::cancel_pending_cloudflare_authorization();
        if !selecting && !pending && self.cloudflare_authorization.lock().unwrap().is_some() {
            bail!(ErrorCode::InvalidRequest.error("authorization completed; deployment continues"));
        }
        encode(relay::end_cloudflare_authorization().await?)
    }

    async fn create_cloudflare_network(
        self: &Arc<Self>,
        request: u64,
        params: Value,
    ) -> Result<Value> {
        params!(
            params,
            worker_name: String,
            account: String,
            device_name: String,
            region: Option<relay::Region>,
            resume: bool,
            progress_id: String,
        );
        let reporter = self.reporter(request, Some(progress_id), "create_cloudflare_network");
        let prompt = self.prompt(request);
        let result: Result<DeployedRelay> = reporter
            .scope(async {
                let prepared = if resume {
                    relay::saved_cloudflare_relay(worker_name.trim())
                } else {
                    relay::prepare_cloudflare_relay(worker_name.trim(), account, &prompt, region)
                        .await
                };
                let deployed = match prepared {
                    Ok(deployed) => deployed,
                    Err(primary) => {
                        if let Err(cleanup) = relay::end_cloudflare_authorization().await {
                            cleanup_warning(&reporter, &cleanup);
                        }
                        return Err(primary);
                    }
                };
                reporter.report(
                    Stage::Deployed,
                    ProgressStatus::Completed,
                    0,
                    ProgressDetails {
                        deployment: Some(deployed.clone()),
                        ..Default::default()
                    },
                );
                // Keep both futures alive: connection proceeds while revocation completes,
                // and no credential-cleanup task is abandoned when a CLI/App future returns.
                settle_connection(
                    self.connect_relay(&deployed, device_name.trim(), &reporter),
                    relay::end_cloudflare_authorization(),
                    &reporter,
                )
                .await?;
                Ok(deployed)
            })
            .await;
        let cancelled = result
            .as_ref()
            .err()
            .is_some_and(|e| crate::error::is(e, ErrorCode::OAuthCancelled));
        reporter.report(
            Stage::Complete,
            if result.is_ok() {
                ProgressStatus::Completed
            } else if cancelled {
                ProgressStatus::Cancelled
            } else {
                ProgressStatus::Failed
            },
            0,
            failure_details(&result),
        );
        self.cloudflare_authorization.lock().unwrap().take();
        if cancelled {
            return Ok(Value::Null);
        }
        encode(result?)
    }

    async fn connect_relay(
        &self,
        deployed: &DeployedRelay,
        name: &str,
        reporter: &DeploymentReporter,
    ) -> Result<()> {
        relay::check_relay(&deployed.link).await?;
        reporter
            .step(
                Stage::NetworkCreation,
                client::connect(&deployed.link, Some(name.to_owned()), false),
            )
            .await?;
        reporter
            .step(Stage::ServiceStart, async {
                services::initialize_daemon()?;
                self.call("start_service").await.map(drop)
            })
            .await
            .map_err(service_start_failed)?;
        reporter
            .step(Stage::ServiceConnection, async {
                tokio::time::timeout(Duration::from_secs(20), async {
                    loop {
                        let status = client::local_status()?;
                        if status.daemon_connected == Some(true) {
                            return Ok::<_, anyhow::Error>(());
                        }
                        if !status.daemon_running {
                            bail!(
                                ErrorCode::ServiceFailed
                                    .error("the service stopped before connecting")
                            );
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                })
                .await
                .map_err(|_| {
                    ErrorCode::ConnectTimeout
                        .error("network created; retry starting the service to connect")
                })?
            })
            .await
            .map_err(service_start_failed)
    }
}

/// Local status for the App's main view; a roster failure keeps its machine code.
fn snapshot() -> Result<Value> {
    let snapshot = client::local_snapshot()?;
    let network_error = match snapshot.network_error {
        Some(Data::Error { code, message }) => Some(Failure { code, message }),
        _ => None,
    };
    Ok(json!({
        "local": snapshot.local,
        "network": snapshot.network,
        "allow_from": snapshot.allow_from,
        "deny_from": snapshot.deny_from,
        "network_error": network_error,
    }))
}

// The network exists at this point; the App offers a service retry without asking
// the user to repeat registration or keep the invitation link.
fn service_start_failed(error: anyhow::Error) -> anyhow::Error {
    CodedError::from_wire("SERVICE_START_FAILED", format!("{error:#}")).into()
}

fn failure_details<T>(result: &Result<T>) -> ProgressDetails {
    ProgressDetails {
        error: result.as_ref().err().map(Data::error),
        ..Default::default()
    }
}

fn cleanup_warning(reporter: &DeploymentReporter, failure: &anyhow::Error) {
    reporter.report(
        Stage::AuthorizationCleanup,
        ProgressStatus::Warning,
        0,
        ProgressDetails {
            error: Some(Data::error(failure)),
            ..Default::default()
        },
    );
}

async fn settle_connection(
    connected: impl Future<Output = Result<()>>,
    cleanup: impl Future<Output = Result<()>>,
    reporter: &DeploymentReporter,
) -> Result<()> {
    let (connected, cleanup) = tokio::join!(connected, cleanup);
    if let Err(primary) = connected {
        if let Err(cleanup) = cleanup {
            cleanup_warning(reporter, &cleanup);
        }
        return Err(primary);
    }
    cleanup.map_err(|failure| {
        ErrorCode::NetworkCreatedCleanupFailed
            .error(format!("network created; {failure:#}"))
            .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[cfg(unix)]
    #[test]
    fn protocol_channel_child() -> Result<()> {
        use std::io::Read;
        if std::env::var_os("XRUN_BRIDGE_PIPE_CHILD").is_some() {
            // Stay alive until the parent closes stdin. No system services or
            // real authorization are involved in this descriptor-lifetime test.
            let _ = std::io::stdin().read(&mut [0u8; 1])?;
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn protocol_output_is_not_inherited_by_child_processes() -> Result<()> {
        use std::{
            io::Read,
            os::fd::AsFd,
            process::{Command, Stdio},
        };
        let (mut input, output) = std::io::pipe()?;
        let channel = duplicate_output(output.as_fd())?;
        drop(output);
        let mut child = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "bridge::tests::protocol_channel_child",
                "--nocapture",
            ])
            .env("XRUN_BRIDGE_PIPE_CHILD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        drop(channel);
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let _ = sender.send(input.read(&mut [0u8; 1]));
        });
        let eof = receiver.recv_timeout(Duration::from_secs(2));
        // Cleanup runs before assertions, including when an inherited writer
        // prevents EOF. wait() closes the child's stdin and allows it to exit.
        let status = child.wait()?;
        reader.join().unwrap();
        assert!(status.success());
        assert_eq!(eof??, 0, "a descendant kept the protocol output open");
        Ok(())
    }

    #[tokio::test]
    async fn connection_runs_before_cleanup_finishes_and_primary_errors_are_preserved() -> Result<()>
    {
        let reporter = DeploymentReporter::new("owned-test".into(), "cloudflare", "create", |_| {});
        let connected = Arc::new(tokio::sync::Notify::new());
        let reached = connected.clone();
        let (release, wait_release) = oneshot::channel();
        let copy = reporter.clone();
        let task = tokio::spawn(async move {
            settle_connection(
                async {
                    reached.notify_one();
                    Ok(())
                },
                async {
                    wait_release.await?;
                    Ok(())
                },
                &copy,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), connected.notified()).await?;
        assert!(!task.is_finished());
        release.send(()).unwrap();
        task.await??;
        let primary = ErrorCode::ConnectTimeout.error("owned connection fixture failed");
        let cleanup = ErrorCode::OAuthRevocationFailed.error("owned cleanup fixture failed");
        let result = settle_connection(
            async { Err(primary.into()) },
            async { Err(cleanup.into()) },
            &reporter,
        )
        .await;
        assert!(crate::error::is(
            &result.unwrap_err(),
            ErrorCode::ConnectTimeout
        ));
        let result = settle_connection(
            async { Ok(()) },
            async {
                Err(ErrorCode::OAuthRevocationFailed
                    .error("owned cleanup fixture failed")
                    .into())
            },
            &reporter,
        )
        .await;
        assert!(crate::error::is(
            &result.unwrap_err(),
            ErrorCode::NetworkCreatedCleanupFailed
        ));
        Ok(())
    }

    struct Connection {
        requests: std::io::PipeWriter,
        responses: std::io::Lines<BufReader<std::io::PipeReader>>,
    }
    impl Connection {
        async fn start() -> Result<(Self, Arc<Bridge>)> {
            let (input, requests) = std::io::pipe()?;
            let (responses, output) = std::io::pipe()?;
            let bridge = Bridge::start(output);
            let served = bridge.clone();
            tokio::spawn(async move { served.listen(BufReader::new(input)).await });
            let mut connection = Self {
                requests,
                responses: BufReader::new(responses).lines(),
            };
            let ready = connection.next().await?;
            assert_eq!(ready["ready"]["version"], crate::protocol::VERSION);
            Ok((connection, bridge))
        }
        async fn exchange(&mut self, message: Value) -> Result<Value> {
            writeln!(self.requests, "{message}")?;
            self.next().await
        }
        async fn next(&mut self) -> Result<Value> {
            let responses = &mut self.responses;
            let line = tokio::task::block_in_place(|| responses.next())
                .ok_or_else(|| anyhow!("bridge closed"))??;
            Ok(serde_json::from_str(&line)?)
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn malformed_and_unknown_requests_fail_with_codes_and_cancellation_reaches_selection()
    -> Result<()> {
        let (mut connection, bridge) = Connection::start().await?;
        let unknown = connection
            .exchange(json!({"id": 1, "method": "format_disk", "params": {}}))
            .await?;
        assert_eq!(unknown["id"], 1);
        assert_eq!(unknown["error"]["code"], "INVALID_REQUEST");
        let mistyped = connection
            .exchange(json!({"id": 2, "method": "pause_access", "params": {"paused": "yes"}}))
            .await?;
        assert_eq!(mistyped["error"]["code"], "INVALID_REQUEST");
        let extra = connection
            .exchange(json!({"id": 3, "method": "suggested_name", "unexpected": true}))
            .await?;
        assert_eq!(extra["id"], 3);
        assert_eq!(extra["error"]["code"], "INVALID_REQUEST");
        let named = connection
            .exchange(json!({"id": 4, "method": "suggested_name"}))
            .await?;
        assert!(crate::protocol::valid_name(
            named["result"].as_str().unwrap()
        ));
        let ping = connection
            .exchange(json!({"id": 5, "method": "ping"}))
            .await?;
        assert_eq!(ping["id"], 5);
        assert!(ping["result"].is_null());
        // Cancelling account selection reaches the pending selection exactly once.
        let (sender, mut receiver) = oneshot::channel();
        *bridge.cloudflare_cancel.lock().unwrap() = Some(sender);
        bridge.cancel_cloudflare_authorization().await?;
        assert_eq!(receiver.try_recv(), Ok(()));
        assert!(bridge.cloudflare_cancel.lock().unwrap().is_none());
        // An App-side failure keeps its code when it returns through a host call.
        let started = tokio::spawn({
            let bridge = bridge.clone();
            async move { bridge.call("start_service").await }
        });
        let call = connection.next().await?;
        assert_eq!(call["method"], "start_service");
        writeln!(
            connection.requests,
            "{}",
            json!({"reply": call["call"], "error": {"code": "HELPER_NOT_FOUND", "message": "reinstall"}})
        )?;
        let error = started.await?.unwrap_err();
        assert_eq!(crate::error::wire(&error).0, "HELPER_NOT_FOUND");
        Ok(())
    }
}
