use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncWriteExt, AsyncBufReadExt, BufReader};
use tokio::time::{timeout, Duration};
use bsm_ipc::telemetry::TelemetryEmitter;
use bsm_ipc::commands::{parse_command, IpcCommand, ResponseEnvelope};

#[tokio::test]
async fn telemetry_subscribe_streams_events() {
    // Bind to ephemeral port
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Shared telemetry broadcaster
    let (tx, _rx) = tokio::sync::broadcast::channel::<String>(8);
    let _telemetry_for_server = TelemetryEmitter::new(tx.clone());
    let telemetry_for_test = TelemetryEmitter::new(tx.clone());

    // Spawn a server task that accepts one client and handles subscribe_telemetry
    tokio::spawn(async move {
        if let Ok((socket, _)) = listener.accept().await {
            let (r, mut w) = socket.into_split();
            let mut reader = BufReader::new(r).lines();

            if let Ok(Some(line)) = reader.next_line().await {
                if let Ok((cmd, id)) = parse_command(&line) {
                    match cmd {
                        IpcCommand::SubscribeTelemetry => {
                            // send ack
                            let ack = ResponseEnvelope::ok(id.clone());
                            let s = ack.to_json().unwrap();
                            let _ = w.write_all(s.as_bytes()).await;
                            let _ = w.write_all(b"\n").await;

                            // start streaming telemetry events from shared broadcaster
                            let mut rx = tx.subscribe();
                            loop {
                                match rx.recv().await {
                                    Ok(msg) => {
                                        if w.write_all(msg.as_bytes()).await.is_err() { break; }
                                        if w.write_all(b"\n").await.is_err() { break; }
                                    }
                                    Err(_) => break,
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    });

    // Connect as client and subscribe
    let mut client = TcpStream::connect(addr).await.unwrap();
    let req = r#"{"cmd":"subscribe_telemetry","id":"s1","payload":{}}"#;
    client.write_all(req.as_bytes()).await.unwrap();
    client.write_all(b"\n").await.unwrap();

    let mut reader = BufReader::new(client);
    let mut line = String::new();

    // Read ack
    let n = timeout(Duration::from_secs(2), reader.read_line(&mut line)).await.unwrap().unwrap();
    assert!(n > 0);
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert!(v["ok"].as_bool().unwrap());

    // Emit an audio_started telemetry event and expect the client to receive it
    line.clear();
    telemetry_for_test.emit_audio_started("test-device".into(), 48000, 2);

    let n2 = timeout(Duration::from_secs(2), reader.read_line(&mut line)).await.unwrap().unwrap();
    assert!(n2 > 0);
    let v2: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v2["event"], "audio_started");
}
