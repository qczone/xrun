//! CLI entry point and local/remote command dispatch.
mod args;
mod execution;
mod files;
mod forward;
mod jobs;
mod local;
mod logs;
mod relay;
mod remote;
mod support;

use crate::{net, protocol::RESERVED};
use args::{DeviceCli, LocalCli, Remote};
use clap::Parser;
use support::{diagnostic, network_error};

pub async fn run() -> i32 {
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
        let ops = [
            "start",
            "info",
            "jobs",
            "wait",
            "logs",
            "kill",
            "push",
            "pull",
            "screenshot",
            "forward",
        ];
        if args
            .get(position)
            .is_none_or(|a| !ops.contains(&a.as_str()))
        {
            args.insert(position, "run".into())
        }
        let cli = DeviceCli::parse_from(args);
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
    let cli = LocalCli::parse_from(args);
    let json = cli.json;
    match Box::pin(local::run(cli)).await {
        Ok(code) => code,
        Err(e) => {
            diagnostic(json, &e);
            125
        }
    }
}
