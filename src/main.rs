#![cfg_attr(
    all(windows, feature = "desktop-helper"),
    windows_subsystem = "windows"
)]

#[tokio::main]
async fn main() {
    let subscriber = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env());
    if std::env::args()
        .take_while(|arg| arg != "--")
        .any(|arg| arg == "--json")
    {
        subscriber.json().init();
    } else {
        subscriber.init();
    }
    std::process::exit(xrun::runtime::cli().await);
}
