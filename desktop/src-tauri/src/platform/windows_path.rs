//! Preserve the user's PATH while adding and removing the installed terminal CLI.
fn entry_key(entry: &str) -> String {
    let entry = entry.trim().trim_matches('"');
    entry
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

fn add(path: &str, directory: &str) -> Option<String> {
    if path
        .split(';')
        .any(|entry| entry_key(entry) == entry_key(directory))
    {
        return None;
    }
    let separator = if path.is_empty() || path.ends_with(';') {
        ""
    } else {
        ";"
    };
    Some(format!("{path}{separator}{directory}"))
}

fn remove(path: &str, directory: &str, separator_added: bool) -> Option<String> {
    let mut offset = 0;
    for entry in path.split(';') {
        let end = offset + entry.len();
        if entry_key(entry) == entry_key(directory) {
            let start = if separator_added && offset > 0 {
                offset - 1
            } else {
                offset
            };
            let end = if start == offset && end < path.len() {
                end + 1
            } else {
                end
            };
            return Some(format!("{}{}", &path[..start], &path[end..]));
        }
        offset = end + 1;
    }
    None
}

#[cfg(windows)]
pub(crate) fn install_cli() -> anyhow::Result<()> {
    registry::configure(true)
}

#[cfg(windows)]
pub(super) fn uninstall_cli() -> anyhow::Result<()> {
    registry::configure(false)
}

#[cfg(windows)]
mod registry {
    use super::{add, entry_key, remove};
    use anyhow::{Context, Result, ensure};
    use serde::{Deserialize, Serialize};
    use std::{fs, io::Write, path::Path};
    use windows_sys::Win32::{
        System::Environment::ExpandEnvironmentStringsW,
        UI::WindowsAndMessaging::{
            HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
        },
    };
    use winreg::{RegKey, RegValue, enums::*, types::ToRegValue};

    #[derive(Serialize, Deserialize)]
    struct Receipt {
        directory: String,
        created_value: bool,
        separator_added: bool,
    }

    pub(super) fn configure(install: bool) -> Result<()> {
        let executable = std::env::current_exe()?;
        let app = executable.parent().context("App directory is missing")?;
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey("Environment")?;
        let changed = if install {
            install_at(&key, app)?
        } else {
            uninstall_at(&key, app)?
        };
        if changed {
            let environment: Vec<u16> = "Environment\0".encode_utf16().collect();
            // Explorer refreshes its environment for newly opened terminals. Existing
            // terminal processes retain their own environment until they are restarted.
            unsafe {
                SendMessageTimeoutW(
                    HWND_BROADCAST,
                    WM_SETTINGCHANGE,
                    0,
                    environment.as_ptr() as isize,
                    SMTO_ABORTIFHUNG,
                    5000,
                    std::ptr::null_mut(),
                );
            }
        }
        Ok(())
    }

    pub(super) fn expand(entry: &str) -> String {
        let input: Vec<u16> = entry.encode_utf16().chain(Some(0)).collect();
        let size = unsafe { ExpandEnvironmentStringsW(input.as_ptr(), std::ptr::null_mut(), 0) };
        if size == 0 {
            return entry.to_owned();
        }
        let mut output = vec![0; size as usize];
        let written =
            unsafe { ExpandEnvironmentStringsW(input.as_ptr(), output.as_mut_ptr(), size) };
        if written == 0 || written > size {
            return entry.to_owned();
        }
        String::from_utf16(&output[..written as usize - 1]).unwrap_or_else(|_| entry.to_owned())
    }

    fn read(key: &RegKey) -> Result<Option<(String, RegValue<'static>)>> {
        let raw = match key.get_raw_value("Path") {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        ensure!(
            matches!(raw.vtype, REG_SZ | REG_EXPAND_SZ),
            "user PATH is not a string"
        );
        ensure!(
            raw.bytes.len() % 2 == 0,
            "user PATH has invalid UTF-16 bytes"
        );
        let words: Vec<u16> = raw
            .bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let words = words.strip_suffix(&[0]).unwrap_or(&words);
        let path = String::from_utf16(words).context("user PATH has invalid UTF-16 text")?;
        ensure!(!path.contains('\0'), "user PATH contains an embedded NUL");
        Ok(Some((path, raw)))
    }

    fn write(key: &RegKey, path: &str, original: Option<&RegValue<'_>>) -> Result<()> {
        let mut value = path.to_reg_value();
        value.vtype = original
            .map(|raw| raw.vtype.clone())
            .unwrap_or(REG_EXPAND_SZ);
        key.set_raw_value("Path", &value).context("save user PATH")
    }

    fn install_at(key: &RegKey, app: &Path) -> Result<bool> {
        let directory = app.join("cli");
        ensure!(
            directory.join("xrun.exe").is_file(),
            "the terminal CLI is missing; reinstall xrun"
        );
        let directory = directory
            .to_str()
            .context("CLI path is not valid Unicode")?;
        ensure!(
            !directory.contains(';'),
            "CLI installation path contains a PATH separator"
        );
        let original = read(key)?;
        let path = original
            .as_ref()
            .map(|(path, _)| path.as_str())
            .unwrap_or("");
        if original
            .as_ref()
            .is_some_and(|(_, raw)| raw.vtype == REG_EXPAND_SZ)
            && add(&expand(path), directory).is_none()
        {
            return Ok(false);
        }
        let Some(updated) = add(path, directory) else {
            return Ok(false);
        };
        let marker = app.join(".xrun-cli-path.json");
        let existed = marker.try_exists()?;
        if existed {
            let receipt: Receipt = serde_json::from_slice(&fs::read(&marker)?)?;
            ensure!(
                entry_key(&receipt.directory) == entry_key(directory),
                "CLI PATH receipt belongs to another location"
            );
        }
        let receipt = Receipt {
            directory: directory.to_owned(),
            created_value: original.is_none(),
            separator_added: !path.is_empty() && !path.ends_with(';'),
        };
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(!existed)
            .truncate(existed)
            .open(&marker)?;
        file.write_all(&serde_json::to_vec(&receipt)?)?;
        file.sync_all()?;
        if let Err(error) = write(key, &updated, original.as_ref().map(|(_, raw)| raw)) {
            if !existed {
                let _ = fs::remove_file(marker);
            }
            return Err(error);
        }
        Ok(true)
    }

    fn uninstall_at(key: &RegKey, app: &Path) -> Result<bool> {
        let marker = app.join(".xrun-cli-path.json");
        let receipt: Receipt = match fs::read(&marker) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let original = read(key)?;
        let mut changed = false;
        if let Some((path, raw)) = original
            && let Some(updated) = remove(&path, &receipt.directory, receipt.separator_added)
        {
            if updated.is_empty() && receipt.created_value {
                key.delete_value("Path")?;
            } else {
                write(key, &updated, Some(&raw))?;
            }
            changed = true;
        }
        fs::remove_file(marker)?;
        Ok(changed)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        struct Fixture {
            key: RegKey,
            name: String,
            app: tempfile::TempDir,
        }
        impl Fixture {
            fn new() -> Result<Self> {
                let app = tempfile::tempdir()?;
                fs::create_dir(app.path().join("cli"))?;
                fs::write(app.path().join("cli/xrun.exe"), b"fixture")?;
                let name = format!(
                    "Software\\xrun\\tests\\{}",
                    app.path().file_name().unwrap().to_string_lossy()
                );
                let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(&name)?;
                Ok(Self { key, name, app })
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&self.name);
            }
        }

        #[test]
        fn registry_preserves_long_unexpanded_path_and_its_value_type() -> Result<()> {
            for kind in [REG_SZ, REG_EXPAND_SZ] {
                let fixture = Fixture::new()?;
                let text = format!("%USERPROFILE%\\工具;{};", "C:\\other;".repeat(1800));
                let mut value = text.to_reg_value();
                value.vtype = kind.clone();
                fixture.key.set_raw_value("Path", &value)?;
                assert!(install_at(&fixture.key, fixture.app.path())?);
                let installed = fixture.key.get_raw_value("Path")?;
                assert_eq!(installed.vtype, kind);
                assert!(!install_at(&fixture.key, fixture.app.path())?);
                assert_eq!(fixture.key.get_raw_value("Path")?.bytes, installed.bytes);
                assert!(uninstall_at(&fixture.key, fixture.app.path())?);
                let restored = fixture.key.get_raw_value("Path")?;
                assert_eq!(restored.vtype, kind);
                assert_eq!(restored.bytes, value.bytes);
            }
            Ok(())
        }

        #[test]
        fn existing_user_entry_is_not_owned_or_removed() -> Result<()> {
            let fixture = Fixture::new()?;
            let path = format!("C:\\other;{}", fixture.app.path().join("cli").display());
            fixture.key.set_value("Path", &path)?;
            assert!(!install_at(&fixture.key, fixture.app.path())?);
            assert!(!fixture.app.path().join(".xrun-cli-path.json").exists());
            assert!(!uninstall_at(&fixture.key, fixture.app.path())?);
            assert_eq!(fixture.key.get_value::<String, _>("Path")?, path);
            Ok(())
        }

        #[test]
        fn uninstall_removes_only_the_owned_entry_and_restores_an_absent_value() -> Result<()> {
            let fixture = Fixture::new()?;
            assert!(install_at(&fixture.key, fixture.app.path())?);
            assert!(uninstall_at(&fixture.key, fixture.app.path())?);
            assert!(read(&fixture.key)?.is_none());
            fixture.key.set_value("Path", &"C:\\before")?;
            assert!(install_at(&fixture.key, fixture.app.path())?);
            let (path, _) = read(&fixture.key)?.unwrap();
            fixture
                .key
                .set_value("Path", &format!("{path};C:\\added-later"))?;
            assert!(uninstall_at(&fixture.key, fixture.app.path())?);
            assert_eq!(
                fixture.key.get_value::<String, _>("Path")?,
                "C:\\before;C:\\added-later"
            );
            Ok(())
        }

        #[test]
        fn reinstall_repairs_a_removed_entry_using_the_current_user_path() -> Result<()> {
            let fixture = Fixture::new()?;
            fixture.key.set_value("Path", &"C:\\before")?;
            assert!(install_at(&fixture.key, fixture.app.path())?);
            fixture.key.set_value("Path", &"C:\\changed;")?;
            assert!(install_at(&fixture.key, fixture.app.path())?);
            assert!(uninstall_at(&fixture.key, fixture.app.path())?);
            assert_eq!(fixture.key.get_value::<String, _>("Path")?, "C:\\changed;");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const CLI: &str = "C:\\Users\\用户\\App with spaces\\cli";

    #[test]
    fn preserves_existing_path_and_deduplicates_windows_directory_spellings() {
        let original = "%USERPROFILE%\\bin;C:\\other";
        let installed = add(original, CLI).unwrap();
        assert_eq!(installed, format!("{original};{CLI}"));
        assert!(add(&installed, CLI).is_none());
        let equivalent = format!("C:\\other;\"{}\\\"", CLI.replace('\\', "/").to_uppercase());
        assert!(add(&equivalent, CLI).is_none());
    }

    #[test]
    fn uninstall_preserves_original_separators_and_changes_from_other_installers() {
        for original in ["", "C:\\other", "C:\\other;", "C:\\other;;"] {
            let installed = add(original, CLI).unwrap();
            let separator_added = !original.is_empty() && !original.ends_with(';');
            assert_eq!(remove(&installed, CLI, separator_added).unwrap(), original);
        }
        let installed = format!("C:\\before;{CLI};C:\\after;{CLI}");
        assert_eq!(
            remove(&installed, CLI, true).unwrap(),
            format!("C:\\before;C:\\after;{CLI}")
        );
        assert!(remove("C:\\unrelated", CLI, true).is_none());
    }

    #[test]
    fn long_paths_are_never_truncated() {
        let original = "%USERPROFILE%\\工具;C:\\other;".repeat(1000);
        let installed = add(&original, CLI).unwrap();
        assert!(installed.starts_with(&original));
        assert_eq!(remove(&installed, CLI, false).unwrap(), original);
    }
}
