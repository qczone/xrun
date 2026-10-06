//! Environment assertions in the shared end-to-end lifecycle.
use super::*;
pub(super) async fn check(suite: &Suite) -> Result<()> {
    let Suite {
        source,
        target,
        runner,
        ..
    } = suite;
    let runner = runner.to_string_lossy();
    let mut env_args = vec!["runner1", "--", &runner, "env-values"];
    env_args.extend(BUILD_ENV);
    env_args.extend(["XRUN_ENV_PRESERVED", "CARGO_HOME", "RUSTUP_HOME", "PATH"]);
    let values = ok(cli(source, &env_args).await);
    let values: std::collections::BTreeMap<_, _> = values
        .lines()
        .map(|line| line.split_once('=').unwrap())
        .collect();
    for &name in BUILD_ENV {
        assert_eq!(values[name], "", "inherited {name} leaked into the task");
    }
    assert_eq!(values["XRUN_ENV_PRESERVED"], "keep");
    assert_eq!(values["CARGO_HOME"], "cargo-home-kept");
    assert_eq!(values["RUSTUP_HOME"], "rustup-home-kept");
    assert!(!values["PATH"].is_empty());

    // Config and per-request values intentionally restore filtered variables;
    // a request must still override the configured value.
    let config_path = target.join(".xrun/daemon.toml");
    let mut task_config: xrun::testing::config::DaemonConfig =
        xrun::testing::config::read(&config_path)?;
    task_config
        .env
        .insert("CARGO_TARGET_DIR".into(), "configured-target".into());
    task_config
        .env
        .insert("RUSTUP_TOOLCHAIN".into(), "configured-toolchain".into());
    xrun::testing::config::write(&config_path, &task_config)?;
    assert_eq!(
        ok(cli(
            source,
            &[
                "runner1",
                "--",
                &runner,
                "env-values",
                "CARGO_TARGET_DIR",
                "RUSTUP_TOOLCHAIN",
            ]
        )
        .await),
        "CARGO_TARGET_DIR=configured-target\nRUSTUP_TOOLCHAIN=configured-toolchain\n"
    );
    assert_eq!(
        ok(cli(
            source,
            &[
                "runner1",
                "--env",
                "CARGO_TARGET_DIR=requested-target",
                "--env",
                if cfg!(windows) {
                    "rustup_toolchain=requested-toolchain"
                } else {
                    "RUSTUP_TOOLCHAIN=requested-toolchain"
                },
                "--",
                &runner,
                "env-values",
                "CARGO_TARGET_DIR",
                "RUSTUP_TOOLCHAIN",
            ]
        )
        .await),
        "CARGO_TARGET_DIR=requested-target\nRUSTUP_TOOLCHAIN=requested-toolchain\n"
    );
    task_config.env.remove("CARGO_TARGET_DIR");
    task_config.env.remove("RUSTUP_TOOLCHAIN");
    xrun::testing::config::write(&config_path, &task_config)?;

    #[cfg(unix)]
    {
        let root = &suite.root;
        let link = root.join("cargo");
        std::os::unix::fs::symlink(runner.as_ref(), &link)?;
        let link = link.to_string_lossy();
        let cwd = root.to_string_lossy();
        let path = format!("PATH={cwd}");
        for args in [
            vec!["runner1", "--", &link, "argv0"],
            vec!["runner1", "--env", &path, "--", "cargo", "argv0"],
            vec!["runner1", "-C", &cwd, "--", "./cargo", "argv0"],
            vec![
                "runner1", "-C", &cwd, "--env", "PATH=.", "--", "cargo", "argv0",
            ],
        ] {
            assert_eq!(ok(cli(source, &args).await), "cargo");
        }
    }

    Ok(())
}
