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
        let direct = ok(cli(home.path(), &["ubuntu", operation, "--help"]).await);
        assert_eq!(
            direct,
            ok(cli(home.path(), &["help", operation]).await),
            "{operation}"
        );
        assert_eq!(
            direct,
            ok(cli(home.path(), &["help", "ubuntu", operation]).await),
            "{operation}"
        );
        assert_eq!(
            direct,
            ok(cli(home.path(), &["ubuntu", "help", operation]).await),
            "{operation}"
        );
    }
    let device = ok(cli(home.path(), &["ubuntu", "--help"]).await);
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
        ok(cli(home.path(), &["help", "relay", "deploy"]).await),
        ok(cli(home.path(), &["relay", "deploy", "--help"]).await)
    );
    // Windows adds .exe to argv[0]; help must keep the documented command name.
    let executables = tempfile::tempdir().unwrap();
    let renamed = executables.path().join("xrun.exe");
    std::fs::copy(common::binary(), &renamed).unwrap();
    for (arguments, expected) in [
        (vec!["--help"], overview),
        (
            vec!["ubuntu", "push", "--help"],
            ok(cli(home.path(), &["help", "push"]).await),
        ),
        (
            vec!["relay", "deploy", "--help"],
            ok(cli(home.path(), &["help", "relay", "deploy"]).await),
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
        &["help", "ubuntu", "missing"],
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
async fn bilingual_manual_and_chapters_are_available_without_identity_or_network() {
    let home = tempfile::tempdir().unwrap();
    let manual = ok(cli(home.path(), &["doc"]).await);
    assert_eq!(manual, include_str!("../docs/usage.md"));
    assert_ne!(manual, include_str!("../README.md"));
    let output = common::command(home.path(), &["doc"])
        .env("LANG", "zh_CN.UTF-8")
        .env("LC_ALL", "zh_CN.UTF-8")
        .env("LANGUAGE", "zh_CN")
        .output()
        .await
        .unwrap();
    assert_eq!(ok(output), manual);
    let help = ok(cli(home.path(), &["doc", "--help"]).await);
    assert!(help.contains("--lang") && help.contains("[default: en]"));
    let topics = [
        ("install", "Installation", "安装"),
        ("quickstart", "Quick start", "开始使用"),
        ("relay", "Relay deployment", "中转部署"),
        ("access", "Access control", "访问权限"),
        ("network", "Network management", "网络管理"),
        ("execute", "Running programs", "执行程序"),
        ("jobs", "Jobs and logs", "任务与日志"),
        ("automation", "Automation and AI", "自动化与 AI"),
        ("files", "Files and screenshots", "文件与截图"),
        ("streaming", "Streaming execution", "流式执行"),
        ("forward", "Port forwarding", "端口转发"),
        ("desktop", "Desktop app", "桌面 App"),
        ("config", "Configuration and services", "配置与服务"),
        ("errors", "Status and troubleshooting", "状态与排错"),
        ("upgrade", "Upgrading and removal", "升级与移除"),
    ];
    for (lang, expected) in [
        ("en", include_str!("../docs/usage.md")),
        ("zh", include_str!("../docs/usage.zh-CN.md")),
    ] {
        assert_eq!(
            ok(cli(home.path(), &["doc", "--lang", lang]).await),
            expected
        );
        let list = ok(cli(home.path(), &["doc", "--list", "--lang", lang]).await);
        if lang == "en" {
            assert_eq!(ok(cli(home.path(), &["doc", "--list"]).await), list);
        } else {
            assert!(list.contains("xrun doc <TOPIC> --lang zh"));
        }
        for (topic, en, zh) in topics {
            let title = if lang == "en" { en } else { zh };
            assert!(list.contains(topic) && list.contains(title));
            let chapter = ok(cli(home.path(), &["doc", topic, "--lang", lang]).await);
            assert!(chapter.starts_with(&format!("## {title}\n")));
            assert!(expected.contains(chapter.trim_end()));
            assert!(!chapter.contains("\n## "));
            if lang == "en" {
                assert_eq!(ok(cli(home.path(), &["doc", topic]).await), chapter);
            }
        }
    }
    assert_eq!(
        ok(cli(home.path(), &["doc", "--lang", "zh-CN"]).await),
        include_str!("../docs/usage.zh-CN.md")
    );
    assert!(
        ok(cli(home.path(), &["doc", "--lang", "zh", "files"]).await)
            .starts_with("## 文件与截图\n")
    );
    for arguments in [
        &["doc", "missing"][..],
        &["doc", "files", "--list"],
        &["doc", "--lang", "fr"],
        &["doc", "--lang"],
        &["doc", "--lang", "zh", "files", "--list"],
    ] {
        let output = cli(home.path(), arguments).await;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    assert!(xrun::protocol::valid_name("guide"));
    assert!(!xrun::protocol::valid_name("doc"));
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}
