use axum::response::Html;

pub(super) enum PageState {
    Received,
    Declined,
    Invalid,
    Finished,
}

pub(super) fn page(state: PageState, chinese: bool, detail: Option<&str>) -> Html<String> {
    let (title, message) = match (&state, chinese) {
        (PageState::Received, true) => ("授权已返回", "后续操作会在 xrun 中继续。"),
        (PageState::Received, false) => (
            "Authorization returned",
            "Continue in xrun to see progress and results.",
        ),
        (PageState::Declined, true) => ("授权未完成", "请回到 xrun 查看错误详情后重试。"),
        (PageState::Declined, false) => (
            "Authorization incomplete",
            "Return to xrun to review the error and retry.",
        ),
        (PageState::Invalid, true) => ("回调无效", "请从 xrun 重新发起授权。"),
        (PageState::Invalid, false) => ("Invalid callback", "Start a new authorization from xrun."),
        (PageState::Finished, true) => ("此授权已结束", "请回到 xrun 查看结果，或重新发起授权。"),
        (PageState::Finished, false) => (
            "Authorization ended",
            "Return to xrun for the result or start a new authorization.",
        ),
    };
    let details = detail.map_or_else(String::new, |detail| {
        format!(
            "<details><summary>{}</summary><pre>{}</pre></details>",
            if chinese {
                "错误详情"
            } else {
                "Error details"
            },
            escape(detail)
        )
    });
    let logo = include_str!("../../../../assets/logos/xrun-wordmark-light.svg");
    let logo = logo.split_once("?>").map_or(logo, |(_, svg)| svg).trim();
    Html(
        include_str!("page.html")
            .replace("%%LANG%%", if chinese { "zh-CN" } else { "en" })
            .replace(
                "%%STATE%%",
                if matches!(state, PageState::Received | PageState::Finished) {
                    "received"
                } else {
                    "failed"
                },
            )
            .replace("%%TITLE%%", title)
            .replace("%%MESSAGE%%", message)
            .replace(
                "%%FOOTER%%",
                if chinese {
                    "可以关闭此页"
                } else {
                    "You may close this page"
                },
            )
            .replace("%%BRAND%%", logo)
            // Provider text is inserted last so it cannot introduce template replacements.
            .replace("%%DETAILS%%", &details),
    )
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
