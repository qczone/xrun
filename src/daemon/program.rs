//! Command validation and platform-specific executable lookup.
use crate::error::ErrorCode;
use crate::protocol::*;
use anyhow::{Result, bail};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub(super) fn validate_stream(request: &StreamExecution) -> Result<()> {
    if !Path::new(&request.cwd).is_absolute() || request.cwd.contains('\0') {
        bail!(ErrorCode::InvalidRequest.error("cwd must be absolute"))
    }
    if request.program.is_empty()
        || request.program.contains('\0')
        || request.args.iter().any(|a| a.contains('\0'))
    {
        bail!(ErrorCode::InvalidRequest.error("invalid command"))
    }
    if request
        .env
        .iter()
        .any(|(k, v)| k.is_empty() || k.contains(['=', '\0']) || v.contains('\0'))
    {
        bail!(ErrorCode::InvalidRequest.error("invalid environment"))
    }
    #[cfg(windows)]
    {
        let mut keys = HashSet::new();
        if request
            .env
            .keys()
            .any(|k| !keys.insert(k.to_ascii_lowercase()))
        {
            bail!(ErrorCode::InvalidRequest.error("duplicate Windows environment key"))
        }
    }
    Ok(())
}
pub(super) fn resolve_program(
    program: &str,
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        resolve_windows_program(program, cwd, env)
    }
    #[cfg(unix)]
    {
        let p = Path::new(program);
        let mut candidates = vec![];
        if p.is_absolute() {
            candidates.push(p.to_path_buf());
        } else if program.contains(std::path::MAIN_SEPARATOR) {
            candidates.push(cwd.join(p));
        } else {
            let path = env
                .get("PATH")
                .cloned()
                .or_else(|| std::env::var("PATH").ok())
                .unwrap_or_default();
            for dir in std::env::split_paths(&path) {
                if !dir.as_os_str().is_empty() {
                    candidates.push(if dir.is_absolute() {
                        dir.join(p)
                    } else {
                        cwd.join(dir).join(p)
                    });
                }
            }
        }
        for path in candidates {
            if path.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if std::fs::metadata(&path)?.permissions().mode() & 0o111 == 0 {
                        continue;
                    }
                }
                // Keep the invoked name/path: rustup and other command shims
                // dispatch on argv[0]. Let the OS follow executable symlinks.
                return Ok(path);
            }
        }
        bail!(ErrorCode::ProgramNotFound.error(program.to_string()))
    }
}

#[cfg(windows)]
fn resolve_windows_program(
    program: &str,
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<PathBuf> {
    let p = Path::new(program);
    let path_value = env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("PATH"))
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let pathext = env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("PATHEXT"))
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var("PATHEXT").ok())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
    let extensions: Vec<String> = pathext
        .split(';')
        .filter(|x| !x.is_empty())
        .map(|x| x.to_string())
        .collect();
    let bases: Vec<PathBuf> = if p.is_absolute() {
        vec![p.to_path_buf()]
    } else if program.contains(['\\', '/']) {
        vec![cwd.join(p)]
    } else {
        std::env::split_paths(&path_value)
            .filter(|d| !d.as_os_str().is_empty())
            .map(|d| {
                if d.is_absolute() {
                    d.join(p)
                } else {
                    cwd.join(d).join(p)
                }
            })
            .collect()
    };
    for base in bases {
        let candidates = if p.extension().is_some() {
            vec![base]
        } else {
            extensions
                .iter()
                .map(|ext| base.with_extension(ext.trim_start_matches('.')))
                .collect()
        };
        for candidate in candidates {
            if candidate.is_file() {
                let ext = candidate
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or_default();
                if ext.eq_ignore_ascii_case("bat") || ext.eq_ignore_ascii_case("cmd") {
                    bail!(ErrorCode::ShellRequired.error(format!("{}", candidate.display())));
                }
                // Preserve ordinary absolute paths for child applications such as
                // Windows PowerShell; canonicalize adds an incompatible \\?\ prefix.
                return Ok(std::path::absolute(candidate)?);
            }
        }
    }
    bail!(ErrorCode::ProgramNotFound.error(program.to_string()))
}
