// Native fixture: exercises process detachment and the daemon control contract
// without loading a real identity, registering a service, or opening the network.
use std::{fs, thread, time::Duration};

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
    fs::write(
        "daemon-runtime.new",
        r#"{"generation":"fixture","connected":false}"#,
    )?;
    fs::rename("daemon-runtime.new", "daemon-runtime.json")?;
    for _ in 0..500 {
        if fs::read("daemon-stop").is_ok_and(|bytes| bytes == b"fixture") {
            fs::remove_file("daemon-runtime.json")?;
            fs::remove_file("daemon-stop")?;
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
