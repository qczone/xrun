//! Channel to the bundled core helper. The App links no core code: each operation runs
//! in `xrun _app-bridge`, a child process that exits when the App closes its input.
//! Messages are single-line JSON objects; see the helper's `src/bridge.rs`.
use super::{
    error::{CommandError, failure},
    platform,
};
use anyhow::Result;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

type Events = Arc<dyn Fn(&str, Value) + Send + Sync>;
type Reply = std::result::Result<Value, CommandError>;

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const HEALTH_INTERVAL: Duration = Duration::from_secs(5);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(10);

enum WaitPolicy {
    Deadline(Duration),
    Responsive {
        interval: Duration,
        timeout: Duration,
    },
}

fn wait_policy(method: &str) -> WaitPolicy {
    match method {
        // These operations have no useful fixed duration. Their own protocol
        // deadlines still apply; a separate probe detects a stalled helper.
        "cloudflare_accounts"
        | "create_cloudflare_network"
        | "logout_cloudflare"
        | "cancel_cloudflare_authorization"
        | "remote_file_upload"
        | "remote_file_download"
        | "remote_file_resume"
        | "remote_screenshot"
        | "save_activity_attachment" => WaitPolicy::Responsive {
            interval: HEALTH_INTERVAL,
            timeout: HEALTH_TIMEOUT,
        },
        _ => WaitPolicy::Deadline(RESPONSE_TIMEOUT),
    }
}

/// Local identity and service summary. Other fields pass through to the webview unchanged.
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct LocalStatus {
    pub(crate) joined: bool,
    pub(crate) name: Option<String>,
    pub(crate) daemon_installed: bool,
    pub(crate) daemon_running: bool,
    pub(crate) daemon_connected: Option<bool>,
    pub(crate) remote_access_paused: bool,
    #[serde(flatten)]
    pub(crate) other: serde_json::Map<String, Value>,
}

/// Local status, verified network and access policy, read without relay IO.
#[derive(Deserialize)]
pub(crate) struct Snapshot {
    pub(crate) local: LocalStatus,
    pub(crate) network: Option<Value>,
    pub(crate) allow_from: Vec<String>,
    pub(crate) deny_from: Vec<String>,
    pub(crate) network_error: Option<CommandError>,
}

/// Whether a daemon holds the instance lock, and the generation it published when ready.
#[derive(Deserialize)]
pub(crate) struct DaemonState {
    pub(crate) running: bool,
    pub(crate) generation: Option<String>,
}

/// Run an operation in the helper. Ordinary requests have a deadline; long-running
/// operations may continue while the helper answers independent health probes.
pub(crate) async fn call(method: &str, params: Value) -> Result<Value> {
    request(method, params, None).await
}

/// Like [`call`], delivering the operation's progress events while it runs.
pub(crate) async fn call_with_events(
    method: &str,
    params: Value,
    events: impl Fn(&str, Value) + Send + Sync + 'static,
) -> Result<Value> {
    request(method, params, Some(Arc::new(events))).await
}

/// Typed [`call`] for results the App inspects.
pub(crate) async fn get<T: DeserializeOwned>(method: &str, params: Value) -> Result<T> {
    decode(call(method, params).await?)
}

/// For native callbacks outside an async context, such as tray menus. The wait is
/// bounded so an unresponsive helper cannot hold the event thread.
pub(crate) fn get_blocking<T: DeserializeOwned>(method: &str, params: Value) -> Result<T> {
    let connection = connection()?;
    get_on_connection(&connection, method, params, HEALTH_TIMEOUT)
}

fn get_on_connection<T: DeserializeOwned>(
    connection: &Connection,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<T> {
    let (sender, receiver) = mpsc::sync_channel(1);
    connection.submit(method, params, Waiter::Thread(sender), None)?;
    match receiver.recv_timeout(timeout) {
        Ok(reply) => decode(reply?),
        Err(_) => {
            connection.close();
            Err(timed_out(method))
        }
    }
}

/// Close the helper and wait for it to exit, so an installer can replace its file next.
pub(crate) fn shutdown() {
    let Some(connection) = CONNECTION.lock().unwrap().take() else {
        return;
    };
    connection.input.lock().unwrap().take();
    if let Some(mut child) = connection.child.lock().unwrap().take() {
        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

async fn request(method: &str, params: Value, events: Option<Events>) -> Result<Value> {
    // Starting a helper includes a blocking signature/version handshake.
    let connection = tauri::async_runtime::spawn_blocking(connection).await??;
    connection
        .request(method, params, events, wait_policy(method))
        .await
}

fn timed_out(method: &str) -> anyhow::Error {
    failure(
        "HELPER_FAILED",
        format!("the xrun helper did not respond to {method}; check the operation before retrying"),
    )
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| {
        failure(
            "HELPER_FAILED",
            format!("unexpected xrun helper response: {e}"),
        )
    })
}

fn stopped() -> CommandError {
    CommandError {
        code: "HELPER_FAILED".into(),
        message: "the xrun helper stopped; retry the operation".into(),
    }
}

/// Operations the helper asks of the App, whose bundle owns service registration.
async fn answer(method: &str) -> Result<Value> {
    match method {
        "start_service" => platform::start().await.map(|()| Value::Null),
        _ => Err(failure(
            "INVALID_REQUEST",
            format!("unknown App operation {method}"),
        )),
    }
}

enum Waiter {
    Thread(mpsc::SyncSender<Reply>),
    Task(tokio::sync::oneshot::Sender<Reply>),
}

impl Waiter {
    fn send(self, reply: Reply) {
        match self {
            Self::Thread(sender) => {
                let _ = sender.send(reply);
            }
            Self::Task(sender) => {
                let _ = sender.send(reply);
            }
        }
    }
}

struct Pending {
    waiter: Waiter,
    events: Option<Events>,
}

struct PendingReply {
    connection: Arc<Connection>,
    id: u64,
    receiver: tokio::sync::oneshot::Receiver<Reply>,
}

impl PendingReply {
    async fn receive(&mut self) -> Result<Value> {
        Ok((&mut self.receiver).await.map_err(|_| stopped())??)
    }
}

impl Drop for PendingReply {
    fn drop(&mut self) {
        self.connection.pending.lock().unwrap().remove(&self.id);
    }
}

struct Connection {
    input: Mutex<Option<mpsc::Sender<Zeroizing<Vec<u8>>>>>,
    pending: Mutex<HashMap<u64, Pending>>,
    next: AtomicU64,
    /// Set when the helper's output ends; the next operation starts another helper.
    closed: AtomicBool,
    child: Mutex<Option<std::process::Child>>,
}

static CONNECTION: Mutex<Option<Arc<Connection>>> = Mutex::new(None);

fn connection() -> Result<Arc<Connection>> {
    let mut current = CONNECTION.lock().unwrap();
    cached_connection(&mut current, connect)
}

fn cached_connection(
    current: &mut Option<Arc<Connection>>,
    connect: impl FnOnce() -> Result<Arc<Connection>>,
) -> Result<Arc<Connection>> {
    if let Some(connection) = current
        .as_ref()
        .filter(|connection| !connection.closed.load(Ordering::Acquire))
    {
        return Ok(connection.clone());
    }
    let connection = connect()?;
    *current = Some(connection.clone());
    Ok(connection)
}

#[cfg(not(test))]
fn connect() -> Result<Arc<Connection>> {
    use std::process::Stdio;
    let mut command = std::process::Command::new(platform::bridge_helper()?);
    command
        .arg("_app-bridge")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn().map_err(|e| {
        failure(
            "HELPER_FAILED",
            format!("could not start the xrun helper: {e}"),
        )
    })?;
    let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        anyhow::bail!(failure("HELPER_FAILED", "the xrun helper has no channel"));
    };
    Connection::start(
        Box::new(input),
        Box::new(std::io::BufReader::new(output)),
        Some(child),
    )
}

// Tests serve the bridge in this process; each isolated case already has its own home.
#[cfg(test)]
fn connect() -> Result<Arc<Connection>> {
    let (input, output) = xrun::testing::bridge::spawn()?;
    Connection::start(Box::new(input), Box::new(output), None)
}

impl Connection {
    fn begin(
        self: &Arc<Self>,
        method: &str,
        params: Value,
        events: Option<Events>,
    ) -> Result<PendingReply> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let id = self.submit(method, params, Waiter::Task(sender), events)?;
        Ok(PendingReply {
            connection: self.clone(),
            id,
            receiver,
        })
    }

    async fn request(
        self: &Arc<Self>,
        method: &str,
        params: Value,
        events: Option<Events>,
        policy: WaitPolicy,
    ) -> Result<Value> {
        let mut reply = self.begin(method, params, events)?;
        match policy {
            WaitPolicy::Deadline(timeout) => self.wait(method, &mut reply, timeout).await,
            WaitPolicy::Responsive { interval, timeout } => {
                tokio::select! {
                    result = reply.receive() => result,
                    error = self.monitor(interval, timeout) => Err(error),
                }
            }
        }
    }

    async fn wait(
        &self,
        method: &str,
        reply: &mut PendingReply,
        timeout: Duration,
    ) -> Result<Value> {
        match tokio::time::timeout(timeout, reply.receive()).await {
            Ok(result) => result,
            Err(_) => {
                self.close();
                Err(timed_out(method))
            }
        }
    }

    async fn monitor(self: &Arc<Self>, interval: Duration, timeout: Duration) -> anyhow::Error {
        loop {
            tokio::time::sleep(interval).await;
            let result = match self.begin("ping", json!({}), None) {
                Ok(mut reply) => self.wait("ping", &mut reply, timeout).await,
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                self.close();
                return error;
            }
        }
    }

    fn start(
        mut input: Box<dyn Write + Send>,
        output: Box<dyn BufRead + Send>,
        child: Option<std::process::Child>,
    ) -> Result<Arc<Self>> {
        let (sender, outgoing) = mpsc::channel::<Zeroizing<Vec<u8>>>();
        let connection = Arc::new(Self {
            input: Mutex::new(Some(sender)),
            pending: Mutex::default(),
            next: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            child: Mutex::new(child),
        });
        // A full pipe must not block the event thread or prevent its timeout
        // from running. Killing a stalled helper also unblocks this writer.
        let writer = Arc::downgrade(&connection);
        std::thread::spawn(move || {
            for line in outgoing {
                if input.write_all(&line).and_then(|()| input.flush()).is_err() {
                    if let Some(connection) = writer.upgrade() {
                        connection.close();
                    }
                    break;
                }
            }
        });
        let (ready, started) = mpsc::sync_channel(1);
        let reader = connection.clone();
        std::thread::spawn(move || reader.read(output, ready));
        // A first launch can wait for the system to verify the helper's signature.
        let started = started
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| Err(failure("HELPER_FAILED", "the xrun helper did not start")));
        if let Err(error) = started {
            connection.close();
            return Err(error);
        }
        Ok(connection)
    }

    fn read(self: Arc<Self>, output: Box<dyn BufRead + Send>, ready: mpsc::SyncSender<Result<()>>) {
        let mut lines = output.lines();
        let version = lines
            .next()
            .and_then(|line| serde_json::from_str::<Value>(&line.ok()?).ok())
            .and_then(|line| Some(line["ready"]["version"].as_str()?.to_owned()));
        let started = match version {
            Some(version) if version == env!("CARGO_PKG_VERSION") => Ok(()),
            Some(_) => Err(failure(
                "HELPER_VERSION_MISMATCH",
                "reinstall the xrun application",
            )),
            None => Err(failure("HELPER_FAILED", "the xrun helper did not start")),
        };
        let serving = started.is_ok();
        let _ = ready.send(started);
        if serving {
            for line in lines {
                let Ok(line) = line else {
                    break;
                };
                if let Ok(message) = serde_json::from_str(&line) {
                    self.receive(message);
                }
            }
        }
        self.close();
    }

    fn receive(self: &Arc<Self>, mut message: Value) {
        if let Some(call) = message.get("call").and_then(Value::as_u64) {
            let method = message["method"].as_str().unwrap_or_default().to_owned();
            let connection = self.clone();
            tauri::async_runtime::spawn(async move {
                let reply = match answer(&method).await {
                    Ok(result) => json!({"reply": call, "result": result}),
                    Err(error) => json!({"reply": call, "error": CommandError::from_error(error)}),
                };
                let _ = connection.write(&reply);
            });
            return;
        }
        let Some(id) = message.get("id").and_then(Value::as_u64) else {
            return;
        };
        if let Some(event) = message
            .get("event")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            let events = self
                .pending
                .lock()
                .unwrap()
                .get(&id)
                .and_then(|pending| pending.events.clone());
            if let Some(events) = events {
                events(&event, message["data"].take());
            }
            return;
        }
        let reply = match message.get_mut("error").map(Value::take) {
            Some(error) => Err(serde_json::from_value(error).unwrap_or_else(|_| stopped())),
            None => Ok(message["result"].take()),
        };
        if let Some(pending) = self.pending.lock().unwrap().remove(&id) {
            pending.waiter.send(reply);
        }
    }

    fn submit(
        &self,
        method: &str,
        params: Value,
        waiter: Waiter,
        events: Option<Events>,
    ) -> Result<u64> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.pending
            .lock()
            .unwrap()
            .insert(id, Pending { waiter, events });
        let mut request = json!({"id": id, "method": method, "params": params});
        let written = self.write(&request);
        // Authority export and restore send a password.
        if let Some(Value::String(password)) = request["params"].get_mut("password") {
            password.zeroize();
        }
        if let Err(error) = written {
            self.pending.lock().unwrap().remove(&id);
            return Err(error);
        }
        Ok(id)
    }

    fn write(&self, message: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        let written = match self.input.lock().unwrap().as_ref() {
            Some(input) => input.send(Zeroizing::new(line)).is_ok(),
            None => {
                line.zeroize();
                false
            }
        };
        if !written {
            self.close();
            return Err(stopped().into());
        }
        Ok(())
    }

    /// Fail operations still in progress; the helper ends once its input closes.
    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.input.lock().unwrap().take();
        let pending: Vec<_> = self.pending.lock().unwrap().drain().collect();
        for (_, pending) in pending {
            pending.waiter.send(Err(stopped()));
        }
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests;
