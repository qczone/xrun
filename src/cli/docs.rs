//! English and Chinese manuals ship in the executable and repository.
use clap::ValueEnum;

#[derive(Clone, Copy, ValueEnum)]
pub(super) enum Language {
    En,
    #[value(alias = "zh-CN")]
    Zh,
}

impl Language {
    fn manual(self) -> &'static str {
        match self {
            Self::En => include_str!("../../docs/usage.md"),
            Self::Zh => include_str!("../../docs/usage.zh-CN.md"),
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub(super) enum Topic {
    Install,
    Quickstart,
    Relay,
    Access,
    Network,
    Execute,
    Jobs,
    Automation,
    Files,
    Streaming,
    Forward,
    Desktop,
    Config,
    Errors,
    Upgrade,
}

impl Topic {
    fn title(self, lang: Language) -> &'static str {
        let (en, zh) = match self {
            Self::Install => ("Installation", "安装"),
            Self::Quickstart => ("Quick start", "开始使用"),
            Self::Relay => ("Relay deployment", "中转部署"),
            Self::Access => ("Access control", "访问权限"),
            Self::Network => ("Network management", "网络管理"),
            Self::Execute => ("Running programs", "执行程序"),
            Self::Jobs => ("Jobs and logs", "任务与日志"),
            Self::Automation => ("Automation and AI", "自动化与 AI"),
            Self::Files => ("Files and screenshots", "文件与截图"),
            Self::Streaming => ("Streaming execution", "流式执行"),
            Self::Forward => ("Port forwarding", "端口转发"),
            Self::Desktop => ("Desktop app", "桌面 App"),
            Self::Config => ("Configuration and services", "配置与服务"),
            Self::Errors => ("Status and troubleshooting", "状态与排错"),
            Self::Upgrade => ("Upgrading and removal", "升级与移除"),
        };
        match lang {
            Language::En => en,
            Language::Zh => zh,
        }
    }
}

pub(super) fn print(topic: Option<Topic>, list: bool, lang: Language) {
    if list {
        for topic in Topic::value_variants() {
            println!(
                "{:<12} {}",
                topic.to_possible_value().unwrap().get_name(),
                topic.title(lang)
            );
        }
        match lang {
            Language::En => println!(
                "\nRead a chapter: xrun doc <TOPIC>\nRead the complete manual: xrun doc\n中文：xrun doc --lang zh"
            ),
            Language::Zh => println!(
                "\n阅读章节：xrun doc <TOPIC> --lang zh\n完整手册：xrun doc --lang zh\nEnglish: xrun doc --lang en"
            ),
        }
    } else if let Some(topic) = topic {
        println!(
            "{}",
            chapter(topic, lang).expect("embedded manual chapter must exist")
        );
    } else {
        print!("{}", lang.manual());
    }
}

fn chapter(topic: Topic, lang: Language) -> Option<&'static str> {
    let manual = lang.manual();
    let heading = format!("\n## {}\n", topic.title(lang));
    let start = manual.find(&heading)? + 1;
    let remaining = &manual[start..];
    let end = remaining.find("\n## ").unwrap_or(remaining.len());
    Some(remaining[..end].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_public_topic_has_one_complete_chapter_in_each_language() {
        for lang in Language::value_variants() {
            let manual = lang.manual();
            assert_eq!(
                manual.matches("\n## ").count(),
                Topic::value_variants().len()
            );
            for topic in Topic::value_variants() {
                let heading = format!("\n## {}\n", topic.title(*lang));
                assert_eq!(manual.matches(&heading).count(), 1);
                let text = chapter(*topic, *lang).expect("missing manual chapter");
                assert!(text.starts_with(&format!("## {}\n", topic.title(*lang))));
                assert!(!text.contains("\n## "));
                assert!(text.lines().count() > 4);
            }
        }
    }
}
