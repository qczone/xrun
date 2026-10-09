mod common;
use anyhow::{Context, Result, bail};
use common::*;
use std::{process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use xrun::testing::{
    crypto, membership::RosterCache, net, protocol::RelayMessage, protocol::*, secure,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn submission_rejection_and_uncertainty_keep_distinct_exit_codes() -> Result<()> {
    use xrun::error::{CodedError, ErrorCode};
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut lab = Lab::new().await?;
        stop_daemon(&lab.source, &mut lab.source_daemon).await?;
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        let cache = RosterCache::open(&lab.target.join(".xrun/roster.db"))?;
        let network = &lab.target_identity.network.as_ref().unwrap().network_id;
        let roster = cache.load(network)?;
        let mut control = relay_socket(
            Some(&lab.target_identity), &roster, &format!("/networks/{network}/control"),
        ).await?;
        let RelayMessage::HelloAck { generation } = net::receive(&mut control).await? else {
            bail!("control acknowledgement")
        };
        let peer = async {
            // The unknown reply must only trigger a query, never a second Exec.
            for step in ["rejected", "unknown", "recovery"] {
                let RelayMessage::Incoming { session_id } = net::receive(&mut control).await? else {
                    bail!("incoming session")
                };
                let mut outer = net::websocket_at(
                    &roster.roster.relay_addresses[0],
                    &format!("/networks/{network}/attach/{}/{generation}/{session_id}", lab.target_identity.device_id),
                    crypto::relay_tls_config(&roster.roster.relay_ca_pem)?,
                ).await?;
                assert!(matches!(net::receive(&mut outer).await?, RelayMessage::Connected { .. }));
                let (mut ws, cert) = secure::server(outer, &lab.target_identity).await?;
                secure::exchange_server(&mut ws, &cache, network, &cert.context("certificate")?).await?;
                assert!(matches!(net::receive(&mut ws).await?, secure::Purpose::Execute));
                net::send(&mut ws, &Data::Ready {
                    version: VERSION.into(), protocol: xrun::protocol::ProtocolRange::CURRENT, selected_protocol: xrun::protocol::PROTOCOL, device_id: lab.target_identity.device_id.clone(),
                    db_id: "test-db".into(), default_cwd: lab.target.to_string_lossy().into(),
                }).await?;
                let Data::Request { request } = net::receive(&mut ws).await? else { bail!("request") };
                let error: anyhow::Error = if step == "recovery" {
                    assert!(matches!(request, Request::Jobs { request_id: Some(ref id), .. } if id == "unknown"));
                    ErrorCode::SourceNotAllowed.error("recovery cannot establish the result").into()
                } else {
                    let Request::Exec { execution, .. } = request else { bail!("expected execution") };
                    assert_eq!(execution.request_id, step);
                    net::receive_bytes(&mut ws, execution.input_size, &execution.input_sha256, MAX_INPUT as u64).await?;
                    if step == "rejected" {
                        ErrorCode::RequestConflict.error("changed validation wording").into()
                    } else {
                        CodedError::from_wire("INVALID_REQUEST_FUTURE", "cannot infer acceptance").into()
                    }
                };
                net::send(&mut ws, &Data::error(&error.context("request handling"))).await?;
                // The CLI may have closed already after consuming the error.
                let _ = ws.close(None).await;
            }
            Ok::<_, anyhow::Error>(())
        };
        let commands = async {
            for (request, expected, diagnostic) in [
                ("rejected", 125, "REQUEST_CONFLICT"),
                ("unknown", 75, "UNCONFIRMED"),
            ] {
                let out = cli(&lab.source, &["target1", "--json", "--request-id", request, "--", "must-not-run"]).await;
                assert_eq!(out.status.code(), Some(expected), "{}", String::from_utf8_lossy(&out.stderr));
                let error: serde_json::Value = serde_json::from_slice(&out.stderr)?;
                assert_eq!(error["code"], diagnostic);
            }
            let recent = json(cli(&lab.source, &["recent", "--json"]).await);
            let submissions = recent.as_array().context("submissions")?;
            assert!(submissions.iter().any(|s| s["request_id"] == "rejected" && s["status"] == "not_accepted"));
            assert!(submissions.iter().any(|s| s["request_id"] == "unknown" && s["status"] == "unconfirmed"));
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(peer, commands)?;
        Ok::<_, anyhow::Error>(())
    }).await?
}

fn job(execution: &Execution, lab: &Lab) -> Job {
    Job {
        job_id: "ABC123".into(),
        request_id: execution.request_id.clone(),
        request_hash: execution.hash(),
        source_device_id: lab.source_identity.device_id.clone(),
        target_device_id: lab.target_identity.device_id.clone(),
        db_id: execution.db_id.clone(),
        state: JobState::Running,
        last_log_seq: 0,
        output_loss_reason: None,
        created_at_ms: now_ms(),
        updated_at_ms: now_ms(),
        leftover_possible: false,
        process: None,
        details: JobDetails::Exec(CommandParams {
            program: execution.program.clone(),
            args: execution.args.clone(),
            cwd: execution.cwd.clone(),
            timeout: 0,
            shell: None,
            input_size: None,
            input_sha256: None,
        }),
        result: None,
        output_complete: Some(true),
        error_code: None,
        error_message: None,
        log_bytes: 0,
        attachments: vec![],
        started_at_ms: None,
        finished_at_ms: if (JobState::Running).terminal() {
            Some(now_ms())
        } else {
            None
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreground_uses_one_session_and_recovers_without_resubmitting() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut lab = Lab::new().await?;
        stop_daemon(&lab.source, &mut lab.source_daemon).await?;
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        let cache = RosterCache::open(&lab.target.join(".xrun/roster.db"))?;
        let network = &lab.target_identity.network.as_ref().unwrap().network_id;
        let roster = cache.load(network)?;
        let mut control = relay_socket(
            Some(&lab.target_identity),
            &roster,
            &format!("/networks/{network}/control"),
        )
        .await?;
        let RelayMessage::HelloAck { generation } = net::receive(&mut control).await? else {
            bail!("control acknowledgement")
        };
        let peer = async {
            // Normal execution has exactly one session. Recovery opens only
            // queries: losing either the acknowledgement or logs never resubmits.
            for failure in ["none", "ack", "logs"] {
                let mut accepted = None;
                let steps: &[&str] = match failure {
                    "ack" => &["exec", "jobs", "logs"],
                    "logs" => &["exec", "logs"],
                    _ => &["exec"],
                };
                for step in steps {
                    let RelayMessage::Incoming { session_id } = net::receive(&mut control).await?
                    else {
                        bail!("incoming session")
                    };
                    let mut outer = net::websocket_at(
                        &roster.roster.relay_addresses[0],
                        &format!(
                            "/networks/{network}/attach/{}/{generation}/{session_id}",
                            lab.target_identity.device_id
                        ),
                        crypto::relay_tls_config(&roster.roster.relay_ca_pem)?,
                    )
                    .await?;
                    assert!(matches!(
                        net::receive(&mut outer).await?,
                        RelayMessage::Connected { .. }
                    ));
                    let (mut ws, cert) = secure::server(outer, &lab.target_identity).await?;
                    secure::exchange_server(
                        &mut ws,
                        &cache,
                        network,
                        &cert.context("certificate")?,
                    )
                    .await?;
                    assert!(matches!(
                        net::receive(&mut ws).await?,
                        secure::Purpose::Execute
                    ));
                    net::send(
                        &mut ws,
                        &Data::Ready {
                            version: VERSION.into(),
                            protocol: xrun::protocol::ProtocolRange::CURRENT,
                            selected_protocol: xrun::protocol::PROTOCOL,
                            device_id: lab.target_identity.device_id.clone(),
                            db_id: "test-db".into(),
                            default_cwd: lab.target.to_string_lossy().into(),
                        },
                    )
                    .await?;
                    let Data::Request { request } = net::receive(&mut ws).await? else {
                        bail!("request")
                    };
                    let after = match (step, request) {
                        (&"exec", Request::Exec { execution, follow }) => {
                            assert!(follow);
                            assert_eq!(execution.request_id, failure);
                            net::receive_bytes(
                                &mut ws,
                                execution.input_size,
                                &execution.input_sha256,
                                MAX_INPUT as u64,
                            )
                            .await?;
                            let value = job(&execution, &lab);
                            accepted = Some(value.clone());
                            if failure == "ack" {
                                ws.close(None).await?;
                                continue;
                            }
                            net::send(&mut ws, &Data::Job { job: value }).await?;
                            0
                        }
                        (&"jobs", Request::Jobs { request_id, .. }) => {
                            assert_eq!(request_id.as_deref(), Some(failure));
                            net::send(
                                &mut ws,
                                &Data::Jobs {
                                    jobs: vec![accepted.clone().unwrap()],
                                    next_offset: None,
                                },
                            )
                            .await?;
                            net::close(&mut ws).await;
                            continue;
                        }
                        (
                            &"logs",
                            Request::Logs {
                                id, after, follow, ..
                            },
                        ) => {
                            assert_eq!(id, "ABC123");
                            assert!(follow);
                            assert_eq!(after, if failure == "logs" { 1 } else { 0 });
                            after
                        }
                        (_, request) => bail!("unexpected request at {step}: {request:?}"),
                    };
                    let mut value = accepted.clone().unwrap();
                    if after == 0 {
                        value.last_log_seq = 1;
                        net::send(
                            &mut ws,
                            &Data::Logs {
                                events: vec![LogEvent {
                                    seq: 1,
                                    stream: "stdout".into(),
                                    data_base64: "YmVmb3JlCg==".into(),
                                }],
                                job: value.clone(),
                            },
                        )
                        .await?;
                    }
                    if failure == "logs" && *step == "exec" {
                        ws.close(None).await?;
                        continue;
                    }
                    value.last_log_seq = 2;
                    value.state = JobState::Failed;
                    value.result = Some(JobResult::Command(CommandResult {
                        exit_code: Some(7),
                        ..Default::default()
                    }));
                    net::send(
                        &mut ws,
                        &Data::Logs {
                            events: vec![LogEvent {
                                seq: 2,
                                stream: "stderr".into(),
                                data_base64: "YWZ0ZXIK".into(),
                            }],
                            job: value,
                        },
                    )
                    .await?;
                    net::send(&mut ws, &Data::End).await?;
                    net::close(&mut ws).await;
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        let commands = async {
            for failure in ["none", "ack", "logs"] {
                let output = cli(
                    &lab.source,
                    &["target1", "--request-id", failure, "--", "test-command"],
                )
                .await;
                assert_eq!(
                    output.status.code(),
                    Some(7),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(output.stdout, b"before\n");
                assert_eq!(output.stderr, b"after\n");
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(peer, commands)?;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execution_survives_log_disconnect_and_follow_does_not_change_request_identity()
-> Result<()> {
    tokio::time::timeout(Duration::from_secs(40), async {
        let mut lab = Lab::new().await?;
        let source = lab.root.path().join("job-child.rs");
        std::fs::write(&source, r#"
use std::{io::Write, time::Duration};
fn main() {
    let args: Vec<String> = std::env::args().collect();
    std::fs::OpenOptions::new().create(true).append(true).open(&args[1]).unwrap().write_all(b"x").unwrap();
    println!("before"); std::io::stdout().flush().unwrap();
    while !std::path::Path::new(&args[2]).exists() { std::thread::sleep(Duration::from_millis(10)); }
    println!("after"); std::process::exit(9);
}"#)?;
        let executable = lab.root.path().join(if cfg!(windows) { "job-child.exe" } else { "job-child" });
        let output = tokio::process::Command::new("rustc").arg(source).arg("-o").arg(&executable).output().await?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let effect = lab.target.join("effect"); let finish = lab.target.join("finish");
        let args = ["target1", "--request-id", "resume", "--timeout", "0", "--", executable.to_str().unwrap(), effect.to_str().unwrap(), finish.to_str().unwrap()];
        let mut child = command(&lab.source, &args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new(); reader.read_line(&mut line).await?;
        assert_eq!(line, "before\n");
        lab.relay.stop().await?;
        std::fs::write(&finish, b"done")?;
        lab.relay.start().await?;
        let mut rest = String::new(); reader.read_to_string(&mut rest).await?;
        assert_eq!(rest, "after\n");
        assert_eq!(child.wait_with_output().await?.status.code(), Some(9));
        let mut background = args.to_vec(); background.insert(1, "start"); background.insert(2, "--json");
        let repeated = json(cli(&lab.source, &background).await);
        assert_eq!(repeated["result"]["exit_code"], 9);
        let output = cli(&lab.source, &args).await;
        assert_eq!(output.status.code(), Some(9));
        assert_eq!(output.stdout, b"before\nafter\n");
        assert_eq!(std::fs::read(&effect)?, b"x");
        Ok::<_, anyhow::Error>(())
    }).await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn differing_release_and_optional_metadata_do_not_prevent_a_negotiated_operation()
-> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut lab = Lab::new().await?;
        stop_daemon(&lab.source, &mut lab.source_daemon).await?;
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        let network = &lab.target_identity.network.as_ref().unwrap().network_id;
        let roster = RosterCache::open(&lab.target.join(".xrun/roster.db"))?.load(network)?;
        let mut control = relay_socket(
            Some(&lab.target_identity),
            &roster,
            &format!("/networks/{network}/control"),
        )
        .await?;
        let RelayMessage::HelloAck { generation } = net::receive(&mut control).await? else {
            bail!("hello")
        };
        let peer = async {
            let RelayMessage::Incoming { session_id } = net::receive(&mut control).await? else {
                bail!("incoming")
            };
            let outer = net::websocket_at(
                &roster.roster.relay_addresses[0],
                &format!(
                    "/networks/{network}/attach/{}/{generation}/{session_id}",
                    lab.target_identity.device_id
                ),
                crypto::relay_tls_config(&roster.roster.relay_ca_pem)?,
            )
            .await?;
            let mut outer = outer;
            assert!(matches!(
                net::receive(&mut outer).await?,
                RelayMessage::Connected { .. }
            ));
            let (mut socket, cert) = secure::server(outer, &lab.target_identity).await?;
            roster.peer(
                &cert.context("certificate")?,
                Some(&lab.source_identity.device_id),
            )?;
            let exchange: serde_json::Value = net::receive(&mut socket).await?;
            assert_eq!(
                exchange["protocol"],
                serde_json::to_value(xrun::protocol::ProtocolRange::CURRENT)?
            );
            assert!(matches!(
                net::receive(&mut socket).await?,
                secure::Purpose::Execute
            ));
            net::send(
                &mut socket,
                &serde_json::json!({
                    "version":"future-release", "protocol":{"min":1,"max":2}, "roster":roster,
                    "diagnostic":"safe optional metadata",
                }),
            )
            .await?;
            net::send(
                &mut socket,
                &serde_json::json!({
                    "type":"ready", "version":"future-release", "protocol":{"min":1,"max":2},
                    "selected_protocol":2, "device_id":lab.target_identity.device_id,
                    "db_id":"test-db", "default_cwd":lab.target.to_string_lossy(),
                    "diagnostic":"safe optional metadata",
                }),
            )
            .await?;
            assert!(matches!(
                net::receive(&mut socket).await?,
                Data::Request {
                    request: Request::Jobs { .. }
                }
            ));
            net::send(
                &mut socket,
                &Data::Jobs {
                    jobs: Vec::new(),
                    next_offset: None,
                },
            )
            .await?;
            net::send(&mut socket, &Data::Complete).await?;
            net::close(&mut socket).await;
            Ok::<_, anyhow::Error>(())
        };
        let caller = async {
            let output = cli(&lab.source, &["target1", "jobs", "--json"]).await;
            assert_eq!(json(output), serde_json::json!([]));
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(peer, caller)?;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipelined_purpose_never_sends_execution_before_handshake_acceptance() -> Result<()> {
    use xrun::testing::membership::Manager;
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut lab = Lab::new().await?;
        stop_daemon(&lab.source, &mut lab.source_daemon).await?;
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        let network = &lab.target_identity.network.as_ref().unwrap().network_id;
        let roster = RosterCache::open(&lab.target.join(".xrun/roster.db"))?.load(network)?;
        let mut control = relay_socket(Some(&lab.target_identity), &roster, &format!("/networks/{network}/control")).await?;
        let RelayMessage::HelloAck { generation } = net::receive(&mut control).await? else { bail!("control acknowledgement") };
        let failures = ["protocol", "signature", "ready-protocol", "ready-identity", "denied", "revoked"];
        let peer = async {
            for failure in failures {
                let RelayMessage::Incoming { session_id } = net::receive(&mut control).await? else { bail!("incoming session") };
                let mut outer = net::websocket_at(&roster.roster.relay_addresses[0], &format!("/networks/{network}/attach/{}/{generation}/{session_id}", lab.target_identity.device_id), crypto::relay_tls_config(&roster.roster.relay_ca_pem)?).await?;
                assert!(matches!(net::receive(&mut outer).await?, RelayMessage::Connected { .. }));
                let (mut ws, cert) = secure::server(outer, &lab.target_identity).await?;
                roster.peer(&cert.context("certificate")?, Some(&lab.source_identity.device_id))?;
                let exchange: serde_json::Value = net::receive(&mut ws).await?;
                assert_eq!(exchange["version"], VERSION);
                // Receiving Purpose before sending our roster proves there is
                // no extra round trip. It must be the only pipelined message.
                assert!(matches!(net::receive(&mut ws).await?, secure::Purpose::Execute));
                assert!(tokio::time::timeout(Duration::from_millis(50), net::receive::<Data>(&mut ws)).await.is_err());
                let mut sent_roster = roster.clone();
                if failure == "signature" { sent_roster.roster.version += 1; }
                if failure == "revoked" {
                    sent_roster = Manager::open(&lab.source.join(".xrun/manager"))?.revoke("target1")?;
                }
                net::send(&mut ws, &serde_json::json!({ "version": "future-release", "protocol": { "min": if failure == "protocol" { 3 } else { 1 }, "max": 3 }, "roster": sent_roster })).await?;
                if failure.starts_with("ready-") || failure == "denied" {
                    // Even a valid roster does not permit sending Exec before Ready.
                    assert!(tokio::time::timeout(Duration::from_millis(50), net::receive::<Data>(&mut ws)).await.is_err());
                    let response = if failure == "denied" {
                        Data::Error { code: "PERMISSION_DENIED".into(), message: "not allowed".into() }
                    } else {
                        Data::Ready { version: "future-release".into(), protocol: xrun::protocol::ProtocolRange::CURRENT, selected_protocol: if failure == "ready-protocol" { 1 } else { xrun::protocol::PROTOCOL }, device_id: if failure == "ready-identity" { lab.source_identity.device_id.clone() } else { lab.target_identity.device_id.clone() }, db_id: "test-db".into(), default_cwd: lab.target.to_string_lossy().into() }
                    };
                    net::send(&mut ws, &response).await?;
                }
                assert!(net::receive::<Data>(&mut ws).await.is_err(), "{failure} must not send an execution request");
            }
            Ok::<_, anyhow::Error>(())
        };
        let commands = async {
            for failure in failures {
                let output = cli(&lab.source, &["target1", "--request-id", failure, "--", "must-not-run"]).await;
                assert_eq!(output.status.code(), Some(125), "{failure}: {}", String::from_utf8_lossy(&output.stderr));
            }
            assert!(json(cli(&lab.source, &["recent", "--json"]).await).as_array().unwrap().is_empty());
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(peer, commands)?;
        Ok::<_, anyhow::Error>(())
    }).await?
}
