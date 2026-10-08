//! Per-user terminal integration for an installed macOS App.
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

const START: &str = "# >>> xrun CLI >>>";
const END: &str = "# <<< xrun CLI <<<";
const PATH_BLOCK: &str = r#"
# >>> xrun CLI >>>
case ":${PATH-}:" in
  *":$HOME/.local/bin:"*) ;;
  *) export PATH="$HOME/.local/bin${PATH:+:$PATH}" ;;
esac
# <<< xrun CLI <<<
"#;

pub(super) fn install(helper: &Path, home: &Path, zsh_directory: Option<&Path>) -> Result<()> {
    ensure!(helper.is_absolute(), "the App helper path must be absolute");
    let executable = |path: &Path| -> Result<bool> {
        let metadata = fs::metadata(path)?;
        Ok(metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    };
    ensure!(executable(helper)?, "the App helper is not executable");
    let link = home.join(".local/bin/xrun");
    match fs::symlink_metadata(&link) {
        Ok(_) => {
            // Keep a standalone CLI or a link installed by another package manager.
            ensure!(
                executable(&link)
                    .with_context(|| format!("existing CLI entry {}", link.display()))?,
                "existing CLI entry {} is not executable",
                link.display()
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(link.parent().context("CLI directory is missing")?)?;
            symlink(helper, &link)?;
        }
        Err(error) => return Err(error.into()),
    }
    let zsh_directory = zsh_directory.unwrap_or(home);
    for name in [".zprofile", ".zshrc"] {
        configure_path(&zsh_directory.join(name))?;
    }
    // Creating .bash_profile would hide an existing .bash_login or .profile.
    let login = [".bash_profile", ".bash_login", ".profile"]
        .into_iter()
        .map(|name| home.join(name))
        .find(|path| path.exists())
        .unwrap_or_else(|| home.join(".profile"));
    configure_path(&login)?;
    configure_path(&home.join(".bashrc"))
}

fn configure_path(path: &Path) -> Result<()> {
    // Dotfiles are often symlinks into a configuration repository. Keep that link.
    let destination = if path.is_symlink() {
        path.canonicalize()
            .with_context(|| format!("resolve shell profile {}", path.display()))?
    } else {
        path.to_path_buf()
    };
    let metadata = match fs::metadata(&destination) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file(),
                "{} is not a regular file",
                path.display()
            );
            Some(metadata)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut contents = if metadata.is_some() {
        fs::read_to_string(&destination)
            .with_context(|| format!("read shell profile {}", path.display()))?
    } else {
        String::new()
    };
    let start = contents.lines().any(|line| line == START);
    let end = contents.lines().any(|line| line == END);
    if start && end {
        return Ok(());
    }
    ensure!(
        !start && !end,
        "incomplete xrun PATH block in {}",
        path.display()
    );
    if let Some(metadata) = &metadata {
        ensure!(
            !metadata.permissions().readonly(),
            "shell profile {} is read-only",
            path.display()
        );
    }
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(PATH_BLOCK);
    let parent = destination
        .parent()
        .context("shell profile directory is missing")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents.as_bytes())?;
    if let Some(metadata) = metadata {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary.as_file().sync_all()?;
    temporary
        .persist(&destination)
        .map_err(|error| error.error)
        .with_context(|| format!("save shell profile {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, process::Command};

    fn helper(root: &Path, version: &str) -> Result<PathBuf> {
        let helper = root.join("App with spaces.app/Contents/MacOS/xrun");
        fs::create_dir_all(helper.parent().unwrap())?;
        fs::write(&helper, format!("#!/bin/sh\nprintf 'xrun {version}\\n'\n"))?;
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))?;
        Ok(helper)
    }

    fn home(root: &Path) -> Result<PathBuf> {
        let home = root.join("Home with spaces");
        fs::create_dir_all(&home)?;
        Ok(home)
    }

    fn shell(home: &Path, zsh: &Path, binary: &str, flags: &str, command: &str) -> Result<String> {
        let output = Command::new(binary)
            .args([flags, command])
            .env_clear()
            .env("HOME", home)
            .env("ZDOTDIR", zsh)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("TERM", "dumb")
            .current_dir(home)
            .output()?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?)
    }

    #[test]
    fn first_launch_makes_cli_available_in_new_login_and_interactive_shells() -> Result<()> {
        let root = tempfile::tempdir()?;
        let home = home(root.path())?;
        let helper = helper(root.path(), "fixture")?;
        fs::write(home.join(".zshrc"), "export EXISTING_SETTING=preserved")?;
        install(&helper, &home, None)?;
        assert_eq!(fs::read_link(home.join(".local/bin/xrun"))?, helper);
        assert!(
            fs::read_to_string(home.join(".zshrc"))?
                .starts_with("export EXISTING_SETTING=preserved\n")
        );
        for (binary, flags) in [
            ("/bin/zsh", "-lc"),
            ("/bin/zsh", "-ic"),
            ("/bin/zsh", "-lic"),
            ("/bin/bash", "-lc"),
            ("/bin/bash", "-ic"),
        ] {
            assert_eq!(
                shell(&home, &home, binary, flags, "xrun --version")?.trim(),
                "xrun fixture"
            );
        }
        assert!(!home.join(".xrun/identity.toml").exists());
        Ok(())
    }

    #[test]
    fn repeated_launches_keep_profiles_unchanged_and_follow_in_place_app_updates() -> Result<()> {
        let root = tempfile::tempdir()?;
        let home = home(root.path())?;
        let binary = helper(root.path(), "before")?;
        install(&binary, &home, None)?;
        let names = [".zprofile", ".zshrc", ".profile", ".bashrc"];
        let before = names.map(|name| fs::read(home.join(name)).unwrap());
        helper(root.path(), "after")?;
        install(&binary, &home, None)?;
        assert_eq!(before, names.map(|name| fs::read(home.join(name)).unwrap()));
        assert_eq!(
            shell(&home, &home, "/bin/zsh", "-lic", "xrun --version")?.trim(),
            "xrun after"
        );
        let path = shell(&home, &home, "/bin/zsh", "-lic", "printf '%s' \"$PATH\"")?;
        assert_eq!(
            path.split(':')
                .filter(|entry| Path::new(entry) == home.join(".local/bin"))
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn existing_standalone_cli_is_not_replaced() -> Result<()> {
        let root = tempfile::tempdir()?;
        let home = home(root.path())?;
        let binary = helper(root.path(), "bundled")?;
        let existing = home.join(".local/bin/xrun");
        fs::create_dir_all(existing.parent().unwrap())?;
        let script = "#!/bin/sh\nprintf 'xrun standalone\\n'\n";
        fs::write(&existing, script)?;
        fs::set_permissions(&existing, fs::Permissions::from_mode(0o755))?;
        install(&binary, &home, None)?;
        assert!(!existing.is_symlink());
        assert_eq!(fs::read_to_string(existing)?, script);
        assert_eq!(
            shell(&home, &home, "/bin/zsh", "-lc", "xrun --version")?.trim(),
            "xrun standalone"
        );
        Ok(())
    }

    #[test]
    fn custom_zsh_directory_dotfile_links_and_bash_login_precedence_are_preserved() -> Result<()> {
        let root = tempfile::tempdir()?;
        let home = home(root.path())?;
        let binary = helper(root.path(), "fixture")?;
        let zsh = home.join("custom zsh");
        fs::create_dir_all(&zsh)?;
        let original = home.join("dotfiles-zshrc");
        fs::write(&original, "export ORIGINAL_SETTING=preserved\n")?;
        fs::set_permissions(&original, fs::Permissions::from_mode(0o640))?;
        symlink(&original, zsh.join(".zshrc"))?;
        fs::write(
            home.join(".bash_login"),
            "export ORIGINAL_LOGIN=preserved\n",
        )?;
        fs::write(home.join(".profile"), "export OTHER_PROFILE=untouched\n")?;
        install(&binary, &home, Some(&zsh))?;
        assert_eq!(fs::read_link(zsh.join(".zshrc"))?, original);
        assert_eq!(fs::metadata(&original)?.permissions().mode() & 0o777, 0o640);
        assert!(fs::read_to_string(original)?.starts_with("export ORIGINAL_SETTING=preserved\n"));
        assert!(!home.join(".bash_profile").exists());
        assert_eq!(
            fs::read_to_string(home.join(".profile"))?,
            "export OTHER_PROFILE=untouched\n"
        );
        for (shell_path, flags) in [("/bin/zsh", "-ic"), ("/bin/bash", "-lc")] {
            assert_eq!(
                shell(&home, &zsh, shell_path, flags, "xrun --version")?.trim(),
                "xrun fixture"
            );
        }
        Ok(())
    }

    #[test]
    fn read_only_profiles_are_preserved_and_the_failure_is_reported() -> Result<()> {
        let root = tempfile::tempdir()?;
        let home = home(root.path())?;
        let binary = helper(root.path(), "fixture")?;
        let profile = home.join(".zprofile");
        let original = "export EXISTING_SETTING=preserved\n";
        fs::write(&profile, original)?;
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o444))?;
        let failure = install(&binary, &home, None).unwrap_err();
        assert!(failure.to_string().contains("read-only"));
        assert_eq!(fs::read_to_string(&profile)?, original);
        assert!(!home.join(".zshrc").exists());
        Ok(())
    }
}
