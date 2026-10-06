mod common;

use common::{cli, ok};

#[tokio::test]
async fn offline_help_routes_local_remote_and_device_queries_without_creating_state() {
    let home = tempfile::tempdir().unwrap();
    let overview = ok(cli(home.path(), &["help"]).await);
    assert_eq!(overview, ok(cli(home.path(), &["--help"]).await));
    assert!(overview.contains("Local commands:") && overview.contains("Remote commands"));
    assert!(overview.contains("xrun doc") && !overview.contains("guide"));

    for operation in [
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
    ] {
        let direct = ok(cli(home.path(), &["linux1", operation, "--help"]).await);
        assert_eq!(
            direct,
            ok(cli(home.path(), &["help", operation]).await),
            "{operation}"
        );
        assert_eq!(
            direct,
            ok(cli(home.path(), &["help", "linux1", operation]).await),
            "{operation}"
        );
        assert_eq!(
            direct,
            ok(cli(home.path(), &["linux1", "help", operation]).await),
            "{operation}"
        );
    }
    let device = ok(cli(home.path(), &["linux1", "--help"]).await);
    assert!(
        device.contains("push") && device.contains("forward") && device.contains("xrun help run")
    );
    let execution = ok(cli(home.path(), &["help", "run"]).await);
    assert!(execution.contains("xrun <DEVICE> [OPTIONS] -- <PROGRAM>"));
    assert!(!execution.contains("<DEVICE> run"));
    assert!(
        execution.contains("Absolute remote working directory") && execution.contains("Exit 75")
    );
    assert_eq!(
        ok(cli(home.path(), &["help", "relay", "install"]).await),
        ok(cli(home.path(), &["relay", "install", "--help"]).await)
    );
    // Windows adds .exe to argv[0]; help must keep the documented command name.
    let executables = tempfile::tempdir().unwrap();
    let renamed = executables.path().join("xrun.exe");
    std::fs::copy(common::binary(), &renamed).unwrap();
    for (arguments, expected) in [
        (vec!["--help"], overview),
        (
            vec!["linux1", "push", "--help"],
            ok(cli(home.path(), &["help", "push"]).await),
        ),
        (
            vec!["relay", "install", "--help"],
            ok(cli(home.path(), &["help", "relay", "install"]).await),
        ),
    ] {
        let mut command = tokio::process::Command::new(&renamed);
        command
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .args(arguments)
            .kill_on_drop(true);
        let output = tokio::time::timeout(std::time::Duration::from_secs(45), command.output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ok(output), expected);
    }
    for arguments in [
        &["help", "missing"][..],
        &["help", "linux1", "missing"],
        &["help", "relay", "missing"],
        &["help", "guide"],
    ] {
        let output = cli(home.path(), arguments).await;
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("unknown help command"));
    }
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn embedded_manual_and_chapters_are_available_without_identity_or_network() {
    let home = tempfile::tempdir().unwrap();
    let manual = ok(cli(home.path(), &["doc"]).await);
    assert_eq!(manual, include_str!("../docs/usage.md"));
    assert_ne!(manual, include_str!("../README.md"));
    let list = ok(cli(home.path(), &["doc", "--list"]).await);
    for (topic, title) in [
        ("install", "安装"),
        ("quickstart", "开始使用"),
        ("access", "访问权限"),
        ("execute", "执行程序"),
        ("jobs", "任务与日志"),
        ("files", "文件与截图"),
        ("streaming", "流式执行"),
        ("forward", "端口转发"),
        ("desktop", "桌面 App"),
        ("relay", "中转部署"),
        ("config", "配置与服务"),
        ("errors", "状态与排错"),
        ("upgrade", "升级与移除"),
    ] {
        assert!(list.contains(topic) && list.contains(title));
        let chapter = ok(cli(home.path(), &["doc", topic]).await);
        assert!(chapter.starts_with(&format!("## {title}\n")));
        assert!(manual.contains(chapter.trim_end()));
        assert!(!chapter.contains("\n## "));
    }
    for arguments in [&["doc", "missing"][..], &["doc", "files", "--list"]] {
        let output = cli(home.path(), arguments).await;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    assert!(xrun::protocol::valid_name("guide"));
    assert!(!xrun::protocol::valid_name("doc"));
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}
