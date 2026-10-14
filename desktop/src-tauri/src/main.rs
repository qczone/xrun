#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub(crate) mod app;
mod authority;
mod commands;
mod connection;
mod error;
mod language;
mod lightweight;
mod platform;
mod remote;
mod tray;

fn main() {
    #[cfg(target_os = "macos")]
    if std::env::args().any(|arg| arg == "--start-service") {
        let result = tauri::async_runtime::block_on(async {
            platform::start().await?;
            println!("{}", serde_json::to_string(&xrun::client::local_status()?)?);
            Ok::<_, anyhow::Error>(())
        });
        if let Err(error) = result {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        return;
    }
    #[cfg(any(windows, target_os = "macos"))]
    if std::env::args().any(|arg| arg == "--install-cli") {
        if let Err(e) = platform::install_cli() {
            println!("{e:#}");
            std::process::exit(1);
        }
        return;
    }
    if std::env::args().any(|arg| arg == "--self-check") {
        let result = tauri::async_runtime::block_on(platform::check_helper()).and_then(|()| {
            let status = xrun::client::local_status()?;
            println!("{}", serde_json::to_string(&status)?);
            Ok(())
        });
        if let Err(e) = result {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
        return;
    }
    #[cfg(windows)]
    if let Some(operation) = std::env::args()
        .find(|arg| matches!(arg.as_str(), "--prepare-update" | "--prepare-uninstall"))
    {
        let result = tauri::async_runtime::block_on(async {
            if operation == "--prepare-uninstall" {
                platform::prepare_uninstall().await
            } else {
                xrun::client::services::stop_daemon().await
            }
        });
        if let Err(e) = result {
            println!("{e:#}");
            std::process::exit(1);
        }
        return;
    }
    let background = std::env::args().any(|arg| arg == "--background");
    app::app_builder(
        tauri::Builder::default().plugin(tauri_plugin_single_instance::init(|app, _, _| {
            tray::present(app)
        })),
        background,
    )
    .build(tauri::generate_context!())
    .expect("failed to initialize xrun desktop")
    .run(app::run_event);
}
