//! Shared, credential-free deployment progress for people, agents and the desktop.
use crate::{error, protocol::Data};
use anyhow::Result;
use serde::Serialize;
use std::{
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

/// Stable deployment phases shared by the CLI and desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentStage {
    /// Waiting for browser consent.
    Authorization,
    /// A validated OAuth callback has returned.
    AuthorizationReceived,
    /// Exchanging or refreshing the authorization grant.
    TokenExchange,
    /// Reading the available accounts.
    Accounts,
    /// An account must be selected explicitly.
    AccountSelection,
    /// Reading or registering the workers.dev subdomain.
    Subdomain,
    /// Checking the existing Worker and its ownership.
    WorkerCheck,
    /// Uploading the bundled Worker.
    WorkerUpload,
    /// Enabling the relay's public address.
    AddressActivation,
    /// The deployment has been saved and can be recovered independently.
    Deployed,
    /// Verifying the relay protocol and transport.
    RelayCheck,
    /// Creating the local network identity.
    NetworkCreation,
    /// Starting the platform service.
    ServiceStart,
    /// Waiting for the daemon's relay connection.
    ServiceConnection,
    /// Revoking temporary Cloudflare credentials.
    AuthorizationCleanup,
    /// The requested operation has completed.
    Complete,
}

/// Status of one deployment phase; completion is distinct from resource creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStatus {
    /// The phase has started.
    Running,
    /// Waiting for a person to authorize or select an account.
    Waiting,
    /// The phase succeeded.
    Completed,
    /// The phase failed.
    Failed,
    /// A secondary failure that does not replace the primary operation error.
    Warning,
    /// The phase was cancelled before completion.
    Cancelled,
}

/// Optional information required to continue or recover an operation.
#[derive(Clone, Default, Serialize)]
pub struct ProgressDetails {
    /// Browser handoff URL, emitted only to the live caller, never persisted in logs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,
    /// Successfully saved deployment, retained even if later connection fails.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<crate::cloudflare::DeployedRelay>,
    /// Accounts available for an explicit selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accounts: Option<Vec<crate::cloudflare::Account>>,
    /// Stable machine code and human-readable failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Data>,
    /// Suggested recovery action, expressed as an argument array rather than shell text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_command: Option<Vec<String>>,
}

/// One JSON progress event. CLI emits these on stderr while stdout remains a final JSON result.
#[derive(Clone, Serialize)]
pub struct DeploymentProgress {
    /// Event discriminator for machine consumers.
    pub event: &'static str,
    /// Caller-supplied identifier; isolates simultaneous or stale UI operations.
    pub operation_id: String,
    /// Provider name.
    pub provider: String,
    /// Operation name.
    pub operation: String,
    /// Monotonically increasing sequence within this operation.
    pub sequence: u64,
    /// UTC wall-clock time in milliseconds, for correlating logs across processes.
    pub timestamp_ms: i64,
    /// Current phase.
    pub stage: DeploymentStage,
    /// Current phase status.
    pub status: ProgressStatus,
    /// Time since the operation started.
    pub elapsed_ms: u64,
    /// Time spent in this phase.
    pub stage_elapsed_ms: u64,
    /// Handoff and recovery information.
    #[serde(flatten)]
    pub details: ProgressDetails,
}

struct State {
    sequence: u64,
    log: Option<PathBuf>,
    interactive: bool,
    deployment: Option<crate::cloudflare::DeployedRelay>,
    completed: std::collections::HashSet<DeploymentStage>,
    started: std::collections::HashSet<DeploymentStage>,
}

/// A task-scoped progress reporter. Spawned tasks do not inherit it implicitly.
#[derive(Clone)]
pub struct DeploymentReporter {
    id: String,
    provider: String,
    operation: String,
    started: Instant,
    state: Arc<Mutex<State>>,
    sink: Arc<dyn Fn(DeploymentProgress) + Send + Sync>,
}

tokio::task_local! { static CURRENT: DeploymentReporter; }

impl DeploymentReporter {
    /// Create a reporter without filesystem side effects.
    pub fn new(
        id: String,
        provider: &str,
        operation: &str,
        sink: impl Fn(DeploymentProgress) + Send + Sync + 'static,
    ) -> Self {
        Self {
            id,
            provider: provider.into(),
            operation: operation.into(),
            started: Instant::now(),
            state: Arc::new(Mutex::new(State {
                sequence: 0,
                log: None,
                interactive: true,
                deployment: None,
                completed: Default::default(),
                started: Default::default(),
            })),
            sink: Arc::new(sink),
        }
    }

    /// Persist a bounded timing log that excludes URLs, credentials and error messages.
    pub fn with_log(self, path: PathBuf) -> Self {
        self.state.lock().unwrap().log = Some(path);
        self
    }

    /// Allow terminal account prompts only for an explicitly interactive caller.
    pub fn with_interactive(self, enabled: bool) -> Self {
        self.state.lock().unwrap().interactive = enabled;
        self
    }

    /// Run an operation with this reporter available to the shared core.
    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        CURRENT.scope(self.clone(), future).await
    }

    /// Run and time one phase, preserving the original typed error.
    pub async fn step<T>(
        &self,
        stage: DeploymentStage,
        future: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let mut guard = Phase {
            reporter: self.clone(),
            stage,
            started: Instant::now(),
            finished: false,
        };
        self.report(
            stage,
            ProgressStatus::Running,
            0,
            ProgressDetails::default(),
        );
        let result = future.await;
        let details = result
            .as_ref()
            .err()
            .map_or_else(ProgressDetails::default, |e| ProgressDetails {
                error: Some(Data::error(e)),
                ..Default::default()
            });
        guard.finished = true;
        self.report(
            stage,
            if result.is_ok() {
                ProgressStatus::Completed
            } else {
                ProgressStatus::Failed
            },
            milliseconds(guard.started),
            details,
        );
        result
    }

    /// Emit a handoff, resource creation or secondary failure event.
    pub fn report(
        &self,
        stage: DeploymentStage,
        status: ProgressStatus,
        stage_elapsed_ms: u64,
        details: ProgressDetails,
    ) {
        let mut state = self.state.lock().unwrap();
        state.sequence += 1;
        if matches!(status, ProgressStatus::Running | ProgressStatus::Waiting) {
            state.started.insert(stage);
            state.completed.remove(&stage);
        }
        if status == ProgressStatus::Completed {
            state.completed.insert(stage);
            if let Some(relay) = &details.deployment {
                state.deployment = Some(relay.clone());
            }
        }
        let event = DeploymentProgress {
            event: "deployment_progress",
            operation_id: self.id.clone(),
            provider: self.provider.clone(),
            operation: self.operation.clone(),
            sequence: state.sequence,
            timestamp_ms: crate::protocol::now_ms(),
            stage,
            status,
            elapsed_ms: milliseconds(self.started),
            stage_elapsed_ms,
            details,
        };
        // A command rejected during argument validation must remain side-effect free.
        let log = if stage == DeploymentStage::Complete
            && state.started.is_empty()
            && state.deployment.is_none()
        {
            None
        } else {
            state.log.clone()
        };
        drop(state);
        if let Some(path) = log {
            log::append(&path, &event);
        }
        (self.sink)(event);
    }
}

fn milliseconds(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

struct Phase {
    reporter: DeploymentReporter,
    stage: DeploymentStage,
    started: Instant,
    finished: bool,
}
impl Drop for Phase {
    fn drop(&mut self) {
        if !self.finished {
            self.reporter.report(
                self.stage,
                ProgressStatus::Cancelled,
                milliseconds(self.started),
                ProgressDetails::default(),
            );
        }
    }
}

pub(crate) fn active() -> bool {
    CURRENT.try_with(|_| ()).is_ok()
}

pub(crate) fn saved_result() -> Option<(crate::cloudflare::DeployedRelay, bool, &'static str)> {
    CURRENT
        .try_with(|reporter| {
            let state = reporter.state.lock().unwrap();
            state.deployment.clone().map(|relay| {
                (
                    relay,
                    state.completed.contains(&DeploymentStage::RelayCheck),
                    if state
                        .completed
                        .contains(&DeploymentStage::AuthorizationCleanup)
                    {
                        "complete"
                    } else if state
                        .started
                        .contains(&DeploymentStage::AuthorizationCleanup)
                    {
                        "unconfirmed"
                    } else {
                        "not_required"
                    },
                )
            })
        })
        .ok()
        .flatten()
}
pub(crate) fn interactive() -> bool {
    CURRENT
        .try_with(|reporter| reporter.state.lock().unwrap().interactive)
        .unwrap_or(true)
}
pub(crate) fn report(stage: DeploymentStage, status: ProgressStatus, details: ProgressDetails) {
    let _ = CURRENT.try_with(|reporter| reporter.report(stage, status, 0, details));
}
pub(crate) async fn step<T>(
    stage: DeploymentStage,
    future: impl Future<Output = Result<T>>,
) -> Result<T> {
    match CURRENT.try_with(Clone::clone) {
        Ok(reporter) => reporter.step(stage, future).await,
        Err(_) => future.await,
    }
}
pub(crate) fn warning(stage: DeploymentStage, failure: &anyhow::Error) {
    report(
        stage,
        ProgressStatus::Warning,
        ProgressDetails {
            error: Some(Data::error(failure)),
            ..Default::default()
        },
    );
    tracing::warn!(stage = ?stage, code = ?error::code(failure), "deployment secondary failure");
}

mod log;
#[cfg(test)]
mod tests;
