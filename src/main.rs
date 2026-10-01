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
    let code = match xrun::cli::run().await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("[xrun] {error:#}");
            125
        }
    };
    std::process::exit(code);
}
