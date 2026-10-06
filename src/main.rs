#![cfg_attr(
    all(windows, feature = "desktop-helper"),
    windows_subsystem = "windows"
)]

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    std::process::exit(xrun::cli::run().await);
}
