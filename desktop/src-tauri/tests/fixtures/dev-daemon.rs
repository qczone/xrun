// Native fixture: exercises process detachment and the daemon control contract
// without loading a real identity, registering a service, or opening the network.
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    thread,
    time::Duration,
};

struct Socket(PathBuf);
impl Drop for Socket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn stop(stream: &mut UnixStream) -> std::io::Result<bool> {
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    let mut header = [0; 2];
    stream.read_exact(&mut header)?;
    if header[0] != 0x81 || header[1] & 0x80 == 0 {
        return Ok(false);
    }
    let size = usize::from(header[1] & 0x7f);
    if size >= 126 {
        return Ok(false);
    }
    let mut mask = [0; 4];
    stream.read_exact(&mut mask)?;
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes)?;
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte ^= mask[i % 4];
    }
    if bytes != br#"{"local":"stop","token":"fixture","generation":"fixture"}"# {
        return Ok(false);
    }
    let reply = br#"{"local":"stopped"}"#;
    stream.write_all(&[0x81, reply.len() as u8])?;
    stream.write_all(reply)?;
    Ok(true)
}

fn main() -> std::io::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!(
            "xrun {}",
            option_env!("XRUN_FIXTURE_VERSION").unwrap_or("fixture")
        );
        return Ok(());
    }
    assert_eq!(std::env::args().nth(1).as_deref(), Some("daemon"));
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open("daemon.lock")?;
    lock.lock()?;
    fs::write("pid", std::process::id().to_string())?;
    // Leave the stale state in place briefly to exercise the readiness check.
    thread::sleep(Duration::from_millis(200));
    // Keep the Unix socket address bounded even when the isolated home is long.
    let path = std::env::temp_dir().join(format!("xrun-fixture-{}.ipc", std::process::id()));
    let listener = UnixListener::bind(&path)?;
    let _socket = Socket(path.clone());
    listener.set_nonblocking(true)?;
    fs::write(
        "daemon-runtime.new",
        r#"{"generation":"fixture","connected":false}"#,
    )?;
    fs::rename("daemon-runtime.new", "daemon-runtime.json")?;
    fs::write(
        "daemon-ipc.json",
        format!(
            r#"{{"address":"{}","token":"fixture","version":"{}","protocol":1}}"#,
            path.display(),
            option_env!("XRUN_FIXTURE_VERSION").unwrap_or("fixture")
        ),
    )?;
    for _ in 0..500 {
        if let Ok((mut stream, _)) = listener.accept()
            && stop(&mut stream)?
        {
            fs::remove_file("daemon-runtime.json")?;
            fs::remove_file("daemon-ipc.json")?;
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
