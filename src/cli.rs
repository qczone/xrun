//! CLI entry point and local/remote command dispatch.
mod args;
mod docs;
mod execution;
mod files;
mod forward;
mod help;
mod jobs;
mod local;
mod logs;
mod network;
mod relay;
mod remote;
mod support;

use crate::{net, protocol::RESERVED};
use args::{DeviceCli, LocalCli, Remote};
use clap::{CommandFactory, Parser};
use support::{diagnostic, network_error};

pub(crate) async fn run() -> i32 {
    let mut args: Vec<String> = std::env::args().collect();
    let first = args
        .iter()
        .skip(1)
        .position(|a| a != "--json")
        .map(|i| i + 1);
    if let Some(first) = first
        && !args[first].starts_with('-')
        && !RESERVED.contains(&args[first].as_str())
    {
        let mut position = first + 1;
        while args.get(position).is_some_and(|a| a == "--json") {
            position += 1;
        }
        let explicit = args.get(position).is_some_and(|argument| {
            matches!(
                argument.as_str(),
                "help" | "-h" | "--help" | "-V" | "--version"
            ) || DeviceCli::command().find_subcommand(argument).is_some()
        });
        if !explicit {
            args.insert(position, "run".into())
        }
        let cli = match parse::<DeviceCli>(args) {
            Ok(cli) => cli,
            Err(code) => return code,
        };
        let json = cli.json;
        let file = matches!(
            &cli.command,
            Remote::Push { .. } | Remote::Pull { .. } | Remote::Screenshot { .. }
        );
        // Keep each command's async state on the heap; Windows has a smaller
        // default main-thread stack, especially visible in debug builds.
        return match Box::pin(remote::run(cli)).await {
            Ok(code) => code,
            Err(e) => {
                diagnostic(json, &e);
                if file && !net::explicit(&e) && !network_error(&e) {
                    1
                } else {
                    125
                }
            }
        };
    }
    let cli = match parse::<LocalCli>(args) {
        Ok(cli) => cli,
        Err(code) => return code,
    };
    let json = cli.json;
    match Box::pin(local::run(cli)).await {
        Ok(code) => code,
        Err(e) => {
            diagnostic(json, &e);
            if crate::error::is(&e, crate::error::ErrorCode::OAuthCancelled) {
                return 130;
            }
            125
        }
    }
}

fn parse<T: Parser>(args: Vec<String>) -> Result<T, i32> {
    let json = args
        .iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| arg == "--json");
    T::try_parse_from(args).map_err(|error| {
        let code = error.exit_code();
        if json && error.use_stderr() {
            diagnostic(
                true,
                &crate::error::ErrorCode::InvalidRequest
                    .error(error.to_string())
                    .into(),
            );
        } else {
            let _ = error.print();
        }
        code
    })
}
