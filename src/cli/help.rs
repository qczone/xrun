//! Offline help routing across the local and device command trees.
use super::args::{DeviceCli, LocalCli};
use anyhow::Result;
use clap::{Command, CommandFactory, Parser};

pub(super) fn run(path: &[String]) -> Result<i32> {
    let mut local = LocalCli::command();
    if path.is_empty() {
        local.print_help()?;
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
    let Some(mut command) = select(root, commands, prefix) else {
        return Ok(unknown(path));
    };
    command.print_help()?;
    Ok(0)
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
