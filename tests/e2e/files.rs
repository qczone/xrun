//! Files assertions in the shared end-to-end lifecycle.
use super::*;
pub(super) async fn check(suite: &Suite) -> Result<()> {
    let Suite { source, target, .. } = suite;
    // Larger than one WebSocket message, with BOM, CRLF and arbitrary bytes.
    let mut content = b"\xef\xbb\xbfhello\r\n".to_vec();
    content.extend((0..2_100_000).map(|i| (i % 256) as u8));
    let remote = target.join("artifact.bin");
    let remote = remote.to_string_lossy();
    ok(input(source, &["runner1", "push", "-", &remote], &content).await);
    let local = source.join("download.bin");
    let local = local.to_string_lossy();
    let pulled = json(cli(source, &["runner1", "pull", &remote, &local, "--json"]).await);
    assert_eq!(pulled["sha256"], sha256(&content));
    assert_eq!(std::fs::read(&*local)?, content);
    #[cfg(unix)]
    {
        let victim = source.join("victim");
        let link = source.join("output-link");
        std::fs::write(&victim, b"keep")?;
        std::os::unix::fs::symlink(&victim, &link)?;
        let refused = cli(
            source,
            &["runner1", "pull", &remote, link.to_str().unwrap()],
        )
        .await;
        assert_eq!(refused.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&refused.stderr).contains("INVALID_PATH"));
        assert_eq!(std::fs::read(&victim)?, b"keep");
    }
    let default = json(cli(source, &["runner1", "pull", &remote, "--json"]).await);
    let temporary = std::path::PathBuf::from(default["path"].as_str().unwrap());
    assert!(
        temporary
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("xrun-pull-")
    );
    assert_eq!(std::fs::read(&temporary)?, content);
    std::fs::remove_file(temporary)?;
    let stale = input(
        source,
        &["runner1", "push", "-", &remote, "--expect", &"0".repeat(64)],
        b"changed",
    )
    .await;
    assert_eq!(stale.status.code(), Some(1));
    assert_eq!(std::fs::read(&*remote)?, content);
    ok(input(
        source,
        &[
            "runner1",
            "push",
            "-",
            &remote,
            "--expect",
            &sha256(&content),
        ],
        b"changed",
    )
    .await);
    assert_eq!(
        cli(source, &["runner1", "pull", &remote, "-"]).await.stdout,
        b"changed"
    );
    assert_eq!(
        cli(source, &["runner1", "pull", &remote, "-", "--json"])
            .await
            .status
            .code(),
        Some(2)
    );
    let exists = input(
        source,
        &["runner1", "push", "-", &remote, "--no-overwrite"],
        b"bad",
    )
    .await;
    assert_eq!(exists.status.code(), Some(1));
    assert_eq!(std::fs::read(&*remote)?, b"changed");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        std::fs::set_permissions(&*remote, std::fs::Permissions::from_mode(0o640))?;
        let link = target.join("link.bin");
        symlink(&*remote, &link)?;
        ok(input(
            source,
            &["runner1", "push", "-", &link.to_string_lossy()],
            b"via link",
        )
        .await);
        assert!(std::fs::symlink_metadata(&link)?.file_type().is_symlink());
        assert_eq!(
            std::fs::metadata(&*remote)?.permissions().mode() & 0o777,
            0o640
        );
    }

    // Snapshots from the first transfer survive subsequent overwrites of the
    // published file. Inspect only this lifecycle's isolated target home.
    let db = rusqlite::Connection::open_with_flags(
        target.join(".xrun/daemon.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let copies = db
        .prepare(
            "SELECT jobs.kind,job_attachments.attachment_id FROM job_attachments JOIN jobs USING(job_id)
         WHERE job_attachments.sha256=?1 AND jobs.state='succeeded'",
        )?
        .query_map([sha256(&content)], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert!(copies.iter().any(|(op, _)| op == "push"));
    assert!(copies.iter().any(|(op, _)| op == "pull"));
    for (_, id) in copies {
        assert_eq!(
            std::fs::read(target.join(".xrun/attachments").join(format!("{id}.blob")))?,
            content
        );
    }
    large_files(suite).await
}

async fn large_files(suite: &Suite) -> Result<()> {
    use std::io::Write;

    let Suite { source, target, .. } = suite;
    let size = 64 * 1024 * 1024 + 1;
    let upload = source.join("large-upload.bin");
    let mut file = std::fs::File::create(&upload)?;
    let chunk = [0xa5; FILE_CHUNK];
    for _ in 0..size / FILE_CHUNK {
        file.write_all(&chunk)?;
    }
    file.write_all(&[0x5a])?;
    drop(file);
    let hash = sha256(&std::fs::read(&upload)?);
    let remote = target.join("large-remote.bin");
    let download = source.join("large-download.bin");
    let db = rusqlite::Connection::open_with_flags(
        target.join(".xrun/daemon.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    for stdin in [false, true] {
        let mut push = command(
            source,
            &[
                "runner1",
                "push",
                if stdin {
                    "-"
                } else {
                    upload.to_str().context("upload path")?
                },
                remote.to_str().context("remote path")?,
                "--json",
            ],
        );
        if stdin {
            push.stdin(Stdio::from(std::fs::File::open(&upload)?));
        }
        let pushed = json(tokio::time::timeout(Duration::from_secs(180), push.output()).await??);
        assert_eq!(pushed["size"], size);
        assert_eq!(pushed["sha256"], hash);
        let pulled = json(
            tokio::time::timeout(
                Duration::from_secs(180),
                command(
                    source,
                    &[
                        "runner1",
                        "pull",
                        remote.to_str().unwrap(),
                        download.to_str().unwrap(),
                        "--json",
                    ],
                )
                .output(),
            )
            .await??,
        );
        assert_eq!(pulled["size"], size);
        assert_eq!(pulled["sha256"], hash);
        assert_eq!(sha256(&std::fs::read(&download)?), hash);
        let pull_ref = pulled["job"].as_str().context("pull job reference")?;
        let finished = json(cli(source, &["runner1", "wait", pull_ref, "--json"]).await);
        assert_eq!(finished["job"]["state"], "succeeded");
        for response in [&pushed, &pulled] {
            let (_, job_id) = response["job"]
                .as_str()
                .context("file job reference")?
                .rsplit_once('/')
                .context("device/job reference")?;
            let (id, bytes, digest): (String, i64, String) = db.query_row(
                "SELECT attachment_id,size_bytes,sha256 FROM job_attachments WHERE job_id=?1",
                [job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(bytes, size as i64);
            assert_eq!(digest, hash);
            assert_eq!(
                sha256(&std::fs::read(
                    target.join(".xrun/attachments").join(format!("{id}.blob"))
                )?),
                hash
            );
        }
    }
    Ok(())
}
