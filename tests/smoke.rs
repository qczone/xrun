mod common;
use anyhow::Result;
use common::*;
use std::time::Duration;
use xrun::protocol::{VERSION, sha256};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn packaged_binary_creates_joins_executes_and_transfers_files() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(45), async {
        println!("smoke binary: {}", binary().display());
        let lab = Lab::new().await?;
        let program = binary().to_string_lossy().into_owned();
        assert_eq!(
            ok(cli(&lab.source, &["target1", "--", &program, "--version"]).await).trim(),
            format!("xrun {VERSION}")
        );
        let mut content = b"\xef\xbb\xbfartifact smoke\r\n".to_vec();
        content.extend((0..1_100_000).map(|i| (i % 256) as u8));
        let local = lab.source.join("upload.bin");
        let remote = lab.target.join("artifact.bin");
        let download = lab.source.join("download.bin");
        std::fs::write(&local, &content)?;
        ok(cli(
            &lab.source,
            &[
                "target1",
                "push",
                &local.to_string_lossy(),
                &remote.to_string_lossy(),
                "--no-overwrite",
            ],
        )
        .await);
        let pulled = json(
            cli(
                &lab.source,
                &[
                    "target1",
                    "pull",
                    &remote.to_string_lossy(),
                    &download.to_string_lossy(),
                    "--json",
                ],
            )
            .await,
        );
        assert_eq!(pulled["sha256"], sha256(&content));
        assert_eq!(sha256(&std::fs::read(&download)?), sha256(&content));
        assert_eq!(std::fs::read(download)?.len(), content.len());
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
