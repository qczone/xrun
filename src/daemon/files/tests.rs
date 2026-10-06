use super::*;
use tokio_tungstenite::tungstenite::protocol::Role;

#[tokio::test]
async fn screenshot_wire_metadata_and_audit_agree_and_disconnects_are_not_successes() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let store = Arc::new(crate::store::TaskStore::open(
        &temp.path().join("tasks.db"),
        true,
    )?);
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 1, 1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&[255, 0, 0])?;
    }
    for disconnected in [false, true] {
        let (a, b) = tokio::io::duplex(4096);
        let mut server = Ws::from_raw_socket(Box::new(a), Role::Server, None).await;
        let mut client = Ws::from_raw_socket(Box::new(b), Role::Client, None).await;
        let mut audit = Some(FileAudit {
            store: store.clone(),
            value: serde_json::json!({"op":"screenshot"}),
            completed: false,
            stream_counts: None,
        });
        let capture = crate::screenshot::Capture {
            bytes: png.clone(),
            width: 1,
            height: 1,
            at: "2026-10-06T00:00:00Z".into(),
        };
        if disconnected {
            drop(client);
            assert!(
                send_capture(&mut server, capture, &mut audit)
                    .await
                    .is_err()
            );
            assert!(!audit.as_ref().unwrap().completed);
        } else {
            send_capture(&mut server, capture, &mut audit).await?;
            let Data::File {
                path,
                size,
                sha256: hash,
                width,
                height,
                captured_at,
            } = net::receive(&mut client).await?
            else {
                bail!("file metadata expected")
            };
            assert!(path.is_empty());
            assert_eq!((width, height), (Some(1), Some(1)));
            assert_eq!(captured_at.as_deref(), Some("2026-10-06T00:00:00Z"));
            assert_eq!(
                net::receive_bytes(&mut client, size, &hash, MAX_FILE).await?,
                png
            );
            assert!(audit.as_ref().unwrap().completed);
        }
        assert_eq!(audit.as_ref().unwrap().value["size"], png.len());
        drop(audit);
    }
    let db = rusqlite::Connection::open(temp.path().join("tasks.db"))?;
    let results = db
        .prepare("SELECT json_extract(data,'$.result') FROM audit ORDER BY rowid")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(results, ["ok", "failed_or_disconnected"]);
    Ok(())
}
