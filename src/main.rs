mod agent;
mod cli;
mod clock;
mod config;
mod crypto;
mod process;
mod protocol;
mod server;
mod store;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    match cli::run().await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            let message = format!("{error:#}");
            if std::env::args().any(|a| a == "--json") {
                let code = message
                    .split([':', ' '])
                    .next()
                    .unwrap_or("EXECUTION_ERROR");
                println!(
                    "{}",
                    serde_json::json!({"type":"error","origin":"xrun","error":{"code":code,"message":message}})
                );
            } else {
                eprintln!("[xrun] {message}");
            }
            std::process::exit(125);
        }
    }
}
