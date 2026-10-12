//! Local encrypted authority actions, serialized with other desktop changes.
use super::{
    app::{Desktop, record},
    error::CommandError,
};
use tauri::State;
use tauri_plugin_dialog::DialogExt;
use xrun::client::network::BackupInfo;

#[tauri::command]
pub(super) async fn choose_authority_archive<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    save: bool,
) -> Result<Option<String>, CommandError> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let dialog = app
        .dialog()
        .file()
        .add_filter("xrun backup", &["xrun-backup"]);
    if save {
        dialog
            .set_file_name("network.xrun-backup")
            .save_file(move |path| {
                let _ = sender.send(path);
            });
    } else {
        dialog.pick_file(move |path| {
            let _ = sender.send(path);
        });
    }
    receiver
        .await
        .map_err(CommandError::from_error)?
        .map(|path| {
            path.into_path()
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(CommandError::from_error)
        })
        .transpose()
}
#[tauri::command]
pub(super) async fn export_authority<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    path: String,
    password: String,
    device: Option<String>,
) -> Result<BackupInfo, CommandError> {
    let _guard = state.action.lock().await;
    let result = tauri::async_runtime::spawn_blocking(move || {
        let password = zeroize::Zeroizing::new(password);
        match device {
            Some(device) => xrun::client::network::migrate_authority(
                &device,
                std::path::Path::new(&path),
                password.as_bytes(),
            ),
            None => xrun::client::network::backup_authority(
                std::path::Path::new(&path),
                password.as_bytes(),
            ),
        }
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|result| result);
    record(&app, result)
}
#[tauri::command]
pub(super) async fn restore_authority<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    path: String,
    password: String,
) -> Result<BackupInfo, CommandError> {
    let _guard = state.action.lock().await;
    let result = tauri::async_runtime::spawn_blocking(move || {
        let password = zeroize::Zeroizing::new(password);
        let restored = xrun::client::network::restore_authority(
            std::path::Path::new(&path),
            password.as_bytes(),
        )?;
        xrun::client::services::initialize_daemon()?;
        Ok::<_, anyhow::Error>(restored)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|result| result);
    record(&app, result)
}
#[tauri::command]
pub(super) async fn retire_authority(state: State<'_, Desktop>) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    xrun::client::network::retire_authority()
        .await
        .map_err(CommandError::from_error)
}

#[cfg(test)]
mod tests {
    use super::super::commands::tests::{build_app, invoke, isolated};
    use anyhow::Result;
    use serde_json::json;
    use xrun::testing::{
        config::{self, Identity, NetworkIdentity},
        membership::{Manager, RosterCache},
    };
    #[test]
    fn native_export_and_restore_publish_only_validated_local_authority() -> Result<()> {
        isolated(
            "native_export_and_restore_publish_only_validated_local_authority",
            || {
                let dir = xrun::client::services::data_dir()?;
                let (manager, member, key_pem, cert_pem) = Manager::create(
                    &dir.join("manager"),
                    "manager1",
                    vec!["https://relay.example/abcdefghijklmnopqrstuvwxyz".into()],
                    String::new(),
                )?;
                let roster = manager.roster()?;
                let identity = Identity {
                    device_id: member.device_id,
                    name: member.name,
                    key_pem,
                    cert_pem,
                    ca_pem: roster.ca_pem.clone(),
                    addresses: roster.roster.relay_addresses.clone(),
                    registration: xrun::protocol::Registration {
                        inviter_id: None,
                        allow_inviter: false,
                    },
                    network: Some(NetworkIdentity {
                        network_id: roster.roster.network_id.clone(),
                        manager_id: roster.roster.manager_id.clone(),
                    }),
                };
                config::write(&dir.join("identity.toml"), &identity)?;
                RosterCache::open(&dir.join("roster.db"))?
                    .observe(&roster.roster.network_id, &roster)?;
                let archive = dir.parent().unwrap().join("native.xrun-backup");
                let (_, window) = build_app();
                let exported = invoke(
                    &window,
                    "export_authority",
                    json!({"path":archive,"password":"owned-native-backup-password","device":null}),
                )
                .unwrap();
                assert_eq!(exported["manager_id"], identity.device_id);
                let failed = invoke(
                    &window,
                    "restore_authority",
                    json!({"path":archive,"password":"incorrect-native-password"}),
                )
                .unwrap_err();
                assert_eq!(failed["code"], "BACKUP_AUTHENTICATION_FAILED");
                assert!(!dir.join("network-state.json").exists());
                let restored = invoke(
                    &window,
                    "restore_authority",
                    json!({"path":archive,"password":"owned-native-backup-password"}),
                )
                .unwrap();
                assert_eq!(restored["authority_generation"], 2);
                assert!(dir.join("network-state.json").exists());
                Ok(())
            },
        )
    }
}
