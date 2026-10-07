//! Private, per-user communication with the existing daemon. The endpoint is
//! random and its advertisement lives in the protected device directory.
use crate::error::ErrorCode;
use crate::{
    config::{self, Identity},
    net::{self, Io, Ws},
    protocol::*,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::protocol::{Role, WebSocketConfig},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    address: String,
    token: String,
    version: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "local", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LocalRequest {
    Open {
        token: String,
        version: String,
        identity: String,
        target: String,
    },
    Stop {
        token: String,
        generation: String,
    },
    ReloadAccess {
        token: String,
    },
    Release,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "local", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LocalResponse {
    Released,
    Stopped,
    AccessReloaded,
}

pub(crate) fn identity_binding(id: &Identity) -> String {
    sha256(format!("{}\0{}\0{}", id.device_id, id.ca_pem, id.cert_pem).as_bytes())
}
async fn framed(io: Io, role: Role) -> Ws {
    WebSocketStream::from_raw_socket(
        net::SocketIo::new(io),
        role,
        Some(
            WebSocketConfig::default()
                .max_frame_size(Some(MAX_MESSAGE))
                .max_message_size(Some(MAX_MESSAGE)),
        ),
    )
    .await
}
pub(crate) async fn connect(id: &Identity, target: &str) -> Result<Option<Ws>> {
    let path = config::device_dir()?.join("daemon-ipc.json");
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let endpoint: Endpoint =
        serde_json::from_slice(&bytes).context("read local daemon endpoint")?;
    if endpoint.version != VERSION {
        bail!(ErrorCode::VersionMismatch.error("restart the local daemon with this release"));
    }
    let io = match tokio::time::timeout(Duration::from_secs(2), open(&endpoint.address)).await {
        Ok(Ok(io)) => io,
        // No request has reached any daemon, so direct connection is safe.
        Ok(Err(e))
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => bail!(ErrorCode::ConnectTimeout.error("local daemon did not accept connection")),
    };
    let mut ws = framed(io, Role::Client).await;
    net::send(
        &mut ws,
        &LocalRequest::Open {
            token: endpoint.token,
            version: VERSION.into(),
            identity: identity_binding(id),
            target: target.into(),
        },
    )
    .await?;
    Ok(Some(ws))
}
#[cfg(unix)]
async fn open(address: &str) -> std::io::Result<Io> {
    let stream = tokio::net::UnixStream::connect(address).await?;
    if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    Ok(Box::new(stream))
}
#[cfg(windows)]
async fn open(address: &str) -> std::io::Result<Io> {
    loop {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(address) {
            Ok(pipe) => return Ok(Box::new(pipe)),
            Err(e) if e.raw_os_error() == Some(231) => {
                tokio::time::sleep(Duration::from_millis(10)).await
            }
            Err(e) => return Err(e),
        }
    }
}

pub(crate) struct Listener {
    path: PathBuf,
    endpoint: Endpoint,
    #[cfg(unix)]
    listener: tokio::net::UnixListener,
    #[cfg(unix)]
    _directory: tempfile::TempDir,
    #[cfg(windows)]
    listener: tokio::net::windows::named_pipe::NamedPipeServer,
}
impl Listener {
    pub(crate) fn bind(dir: &Path) -> Result<Self> {
        config::restrict_dir(dir)?;
        let token = uuid::Uuid::new_v4().to_string();
        #[cfg(unix)]
        let (address, listener, directory) = {
            let directory = tempfile::Builder::new().prefix("xrun-ipc-").tempdir()?;
            config::restrict_dir(directory.path())?;
            let socket = directory.path().join("s");
            let listener = tokio::net::UnixListener::bind(&socket)?;
            (socket.to_string_lossy().into_owned(), listener, directory)
        };
        #[cfg(windows)]
        let (address, listener) = {
            let address = format!(r"\\.\pipe\xrun-{}", uuid::Uuid::new_v4());
            let listener = pipe(&address, true)?;
            (address, listener)
        };
        let endpoint = Endpoint {
            address,
            token,
            version: VERSION.into(),
        };
        let path = dir.join("daemon-ipc.json");
        config::atomic_private_write(&path, &serde_json::to_vec(&endpoint)?)?;
        Ok(Self {
            path,
            endpoint,
            listener,
            #[cfg(unix)]
            _directory: directory,
        })
    }
    pub(crate) async fn accept(&mut self) -> Result<Ws> {
        #[cfg(unix)]
        let io: Io = {
            let (stream, _) = self.listener.accept().await?;
            if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
                bail!(ErrorCode::PermissionDenied.error("local user differs"));
            }
            Box::new(stream)
        };
        #[cfg(windows)]
        let io: Io = {
            self.listener.connect().await?;
            let next = pipe(&self.endpoint.address, false)?;
            Box::new(std::mem::replace(&mut self.listener, next))
        };
        Ok(framed(io, Role::Server).await)
    }
    pub(crate) fn token(&self) -> String {
        self.endpoint.token.clone()
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        if std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Endpoint>(&b).ok())
            .is_some_and(|e| e.token == self.endpoint.token)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
#[cfg(windows)]
fn pipe(address: &str, first: bool) -> Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            SECURITY_ATTRIBUTES,
        },
    };
    let sddl: Vec<u16> = "D:P(A;;GA;;;OW)(A;;GA;;;SY)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let result = unsafe {
        tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .max_instances(64)
            .create_with_security_attributes_raw(
                address,
                (&mut attrs as *mut SECURITY_ATTRIBUTES).cast(),
            )
    };
    unsafe {
        LocalFree(descriptor);
    }
    result.context("create private daemon pipe")
}

pub(crate) async fn stop(dir: &Path, generation: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let endpoint: Endpoint =
            serde_json::from_slice(&std::fs::read(dir.join("daemon-ipc.json"))?)?;
        let mut ws = framed(open(&endpoint.address).await?, Role::Client).await;
        net::send(
            &mut ws,
            &LocalRequest::Stop {
                token: endpoint.token,
                generation: generation.into(),
            },
        )
        .await?;
        match net::receive::<serde_json::Value>(&mut ws).await? {
            value
                if matches!(
                    serde_json::from_value::<LocalResponse>(value.clone()),
                    Ok(LocalResponse::Stopped)
                ) =>
            {
                Ok(())
            }
            value => {
                if let Ok(Data::Error { code, message }) = serde_json::from_value(value) {
                    bail!(crate::error::CodedError::from_wire(code, message));
                }
                bail!(ErrorCode::InvalidMessage.error("expected stop acknowledgement"));
            }
        }
    })
    .await
    .context(ErrorCode::DaemonStopTimeout.error("local daemon did not acknowledge shutdown"))?
}

pub(crate) async fn refresh_access(dir: &Path) -> Result<()> {
    tokio::time::timeout(CONNECT_TIMEOUT, async {
        let endpoint: Endpoint =
            serde_json::from_slice(&std::fs::read(dir.join("daemon-ipc.json"))?)?;
        let mut ws = framed(open(&endpoint.address).await?, Role::Client).await;
        net::send(
            &mut ws,
            &LocalRequest::ReloadAccess {
                token: endpoint.token,
            },
        )
        .await?;
        let value: serde_json::Value = net::receive(&mut ws).await?;
        if let Ok(Data::Error { code, message }) = serde_json::from_value(value.clone()) {
            bail!(crate::error::CodedError::from_wire(code, message));
        }
        match serde_json::from_value(value)? {
            LocalResponse::AccessReloaded => Ok(()),
            _ => bail!(ErrorCode::InvalidMessage.error("expected authorization acknowledgement")),
        }
    })
    .await?
}
