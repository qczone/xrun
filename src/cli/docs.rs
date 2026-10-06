//! The same user manual ships in the repository, CLI archive and executable.
use clap::ValueEnum;

const MANUAL: &str = include_str!("../../docs/usage.md");

#[derive(Clone, Copy, ValueEnum)]
pub(super) enum Topic {
    Install,
    Quickstart,
    Access,
    Execute,
    Jobs,
    Files,
    Streaming,
    Forward,
    Desktop,
    Relay,
    Config,
    Errors,
    Upgrade,
}

impl Topic {
    fn title(self) -> &'static str {
        match self {
            Self::Install => "安装",
            Self::Quickstart => "开始使用",
            Self::Access => "访问权限",
            Self::Execute => "执行程序",
            Self::Jobs => "任务与日志",
            Self::Files => "文件与截图",
            Self::Streaming => "流式执行",
            Self::Forward => "端口转发",
            Self::Desktop => "桌面 App",
            Self::Relay => "中转部署",
            Self::Config => "配置与服务",
            Self::Errors => "状态与排错",
            Self::Upgrade => "升级与移除",
        }
    }
}

pub(super) fn print(topic: Option<Topic>, list: bool) {
    if list {
        for topic in Topic::value_variants() {
            println!(
                "{:<12} {}",
                topic.to_possible_value().unwrap().get_name(),
                topic.title()
            );
        }
        println!("\nRead a chapter: xrun doc <TOPIC>\nRead the complete manual: xrun doc");
    } else if let Some(topic) = topic {
        println!(
            "{}",
            chapter(topic).expect("embedded manual chapter must exist")
        );
    } else {
        print!("{MANUAL}");
    }
}

fn chapter(topic: Topic) -> Option<&'static str> {
    let heading = format!("\n## {}\n", topic.title());
    let start = MANUAL.find(&heading)? + 1;
    let remaining = &MANUAL[start..];
    let end = remaining.find("\n## ").unwrap_or(remaining.len());
    Some(remaining[..end].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_public_topic_has_one_complete_chapter() {
        for topic in Topic::value_variants() {
            let heading = format!("\n## {}\n", topic.title());
            assert_eq!(MANUAL.matches(&heading).count(), 1);
            let text = chapter(*topic).expect("missing manual chapter");
            assert!(text.starts_with(&format!("## {}\n", topic.title())));
            assert!(!text.contains("\n## "));
            assert!(text.lines().count() > 4);
        }
    }
}
