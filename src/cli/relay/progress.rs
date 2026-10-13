use super::{RelayCommand, RelayProvider};
use crate::{
    config,
    deployment::{
        DeploymentProgress, DeploymentReporter, DeploymentStage as Stage, ProgressStatus,
    },
};
use anyhow::Result;

pub(super) fn reporter(command: &RelayCommand, json: bool) -> Result<DeploymentReporter> {
    let (provider, operation) = match command {
        RelayCommand::Deploy { provider, .. } => (provider, "deploy"),
        RelayCommand::Login { provider, .. } => (provider, "login"),
        RelayCommand::Logout { provider } => (provider, "logout"),
        RelayCommand::Run { provider } => (provider, "run"),
        RelayCommand::Link { provider, .. } => (provider, "link"),
        RelayCommand::Check { provider, .. } => (provider, "check"),
        RelayCommand::Remove { provider, .. } => (provider, "remove"),
    };
    let reporter = DeploymentReporter::new(
        uuid::Uuid::new_v4().to_string(),
        if *provider == RelayProvider::Cloudflare {
            "cloudflare"
        } else {
            "local"
        },
        operation,
        move |event| {
            if json {
                eprintln!("{}", serde_json::to_string(&event).unwrap());
            } else {
                human(&event);
            }
        },
    )
    .with_interactive(!json);
    if matches!(
        command,
        RelayCommand::Deploy { dry_run: true, .. }
            | RelayCommand::Link { .. }
            | RelayCommand::Run { .. }
    ) {
        Ok(reporter)
    } else {
        Ok(reporter.with_log(config::device_dir()?.join("deployment.log")))
    }
}

pub(super) fn deployment_result(error: Option<&anyhow::Error>) -> Result<()> {
    if let Some((relay, ready, cleanup)) = crate::deployment::saved_result() {
        let mut output = serde_json::to_value(&relay)?;
        output["deployed"] = true.into();
        output["ready"] = ready.into();
        output["authorization_cleanup"] = cleanup.into();
        output["next_command"] = if ready {
            serde_json::json!(["xrun", "up", "--relay", relay.link, "--json"])
        } else {
            serde_json::json!([
                "xrun",
                "relay",
                "check",
                "--provider",
                "cloudflare",
                "--name",
                relay.name,
                "--json"
            ])
        };
        if let Some(error) = error {
            output["error"] = serde_json::to_value(crate::protocol::Data::error(error))?;
        }
        println!("{output}");
    }
    Ok(())
}

fn human(event: &DeploymentProgress) {
    let label = match event.stage {
        Stage::Authorization => "Waiting for Cloudflare authorization",
        Stage::AuthorizationReceived => "Authorization returned",
        Stage::TokenExchange => "Verifying authorization",
        Stage::Accounts => "Reading Cloudflare accounts",
        Stage::AccountSelection => "Choose an account with --account",
        Stage::Subdomain => "Preparing workers.dev",
        Stage::WorkerCheck => "Checking Worker ownership",
        Stage::WorkerUpload => "Deploying the bundled Worker",
        Stage::AddressActivation => "Enabling the relay address",
        Stage::Deployed => "Worker saved",
        Stage::RelayCheck => "Checking relay connectivity",
        Stage::NetworkCreation => "Creating the network",
        Stage::ServiceStart => "Starting the service",
        Stage::ServiceConnection => "Connecting to the relay",
        Stage::AuthorizationCleanup => "Revoking authorization",
        Stage::Complete => "Operation complete",
    };
    match event.status {
        ProgressStatus::Running | ProgressStatus::Waiting => eprintln!("[xrun] {label}…"),
        ProgressStatus::Failed | ProgressStatus::Warning => {
            eprintln!("[xrun] {label}: {:?}", event.status)
        }
        ProgressStatus::Completed if event.stage == Stage::Deployed => {
            if let Some(relay) = &event.details.deployment {
                eprintln!(
                    "[xrun] Worker deployed: {}. Saved locally; connection failure does not require a new Worker.",
                    relay.link
                );
            }
        }
        ProgressStatus::Completed if event.stage_elapsed_ms >= 1000 => {
            eprintln!(
                "[xrun] {label}: {:.1}s",
                event.stage_elapsed_ms as f64 / 1000.0
            );
        }
        _ => {}
    }
    if let Some(url) = &event.details.authorization_url {
        eprintln!("[xrun] Open in a browser on this machine: {url}");
    }
    if let Some(accounts) = &event.details.accounts {
        for (index, account) in accounts.iter().enumerate() {
            eprintln!("  {}. {} ({})", index + 1, account.name, account.id);
        }
    }
}
