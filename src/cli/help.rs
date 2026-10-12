//! Offline help routing across the local and device command trees.
use super::args::{DeviceCli, LocalCli};
use anyhow::{Context, Result};
use clap::{Command, CommandFactory, Parser, error::ErrorKind};

pub(super) fn run(path: &[String]) -> Result<i32> {
    let local = LocalCli::command();
    if path.is_empty() {
        print_help(local)?;
        return Ok(0);
    }
    let remote = DeviceCli::command();
    let (root, commands, prefix) = if local.find_subcommand(&path[0]).is_some() {
        (local, path, "xrun")
    } else if remote.find_subcommand(&path[0]).is_some() {
        (remote, path, "xrun <DEVICE>")
    } else if path.len() > 1 && DeviceCli::try_parse_from(["xrun", &path[0], "info"]).is_ok() {
        (remote, &path[1..], "xrun <DEVICE>")
    } else {
        return Ok(unknown(path));
    };
    let Some(command) = select(root, commands, prefix) else {
        return Ok(unknown(path));
    };
    print_help(command)?;
    Ok(0)
}

fn print_help(command: Command) -> Result<()> {
    // Let Clap choose the same short/long layout as the direct --help entry.
    let error = command
        .try_get_matches_from(["xrun", "--help"])
        .err()
        .context("command did not handle its help flag")?;
    if error.kind() != ErrorKind::DisplayHelp {
        return Err(error.into());
    }
    error.print()?;
    Ok(())
}

fn select(mut command: Command, path: &[String], prefix: &str) -> Option<Command> {
    command.build();
    for part in path {
        command = command.find_subcommand(part)?.clone();
    }
    Some(command.bin_name(format!("{prefix} {}", path.join(" "))))
}

fn unknown(path: &[String]) -> i32 {
    eprintln!(
        "error: unknown help command '{}'\n\nUse xrun help for commands or xrun doc --list for manual chapters.",
        path.join(" ")
    );
    2
}
