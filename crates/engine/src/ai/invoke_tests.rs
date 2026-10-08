#![allow(clippy::unwrap_used)] // Synthetic test setup helpers.
use super::invoke::*;
use super::*;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;

fn api() -> Value {
    json!({"paths":{
        "/api/v1/queue/{queue_id}/enqueue_batch":{"post":{}},
        "/api/v1/queue/{queue_id}/i/{item_id}":{"get":{}},
        "/api/v1/queue/{queue_id}/i/{item_id}/cancel":{"put":{}},
        "/api/v1/images/upload":{"post":{}},"/api/v1/images/i/{image_name}/full":{"get":{}}
    },"components":{"schemas":{"Graph":{"properties":{"nodes":{"additionalProperties":{"oneOf":[{"$ref":"#/components/schemas/Mock"}]}}}},"Mock":{"properties":{"id":{"type":"string"},"type":{"const":"mock"},"prompt":{"type":"string"},"seed":{"type":"integer"},"width":{"type":"integer"},"height":{"type":"integer"},"is_intermediate":{"type":"boolean"}},"output":{"$ref":"#/components/schemas/ImageOutput"}},"ImageOutput":{"properties":{"image":{"type":"object"}}}}}})
}
struct Response {
    path: &'static str,
    status: u16,
    body: Vec<u8>,
}
fn response(path: &'static str, body: Value) -> Response {
    Response { path, status: 200, body: serde_json::to_vec(&body).unwrap() }
}
fn health() -> Vec<Response> {
    vec![
        response("GET /openapi.json", api()),
        response("GET /api/v1/app/version", json!({"version":"mock"})),
        response("GET /api/v1/queue/default/status", json!({"queue":{}})),
    ]
}
fn server(responses: Vec<Response>) -> (Settings, std::thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut data = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let n = socket.read(&mut buffer).unwrap();
                assert!(n > 0);
                data.extend_from_slice(&buffer[..n]);
                if let Some(end) = data.windows(4).position(|v| v == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&data[..end]);
                    let length = headers
                        .lines()
                        .find_map(|l| l.to_lowercase().strip_prefix("content-length:").and_then(|s| s.trim().parse::<usize>().ok()))
                        .unwrap_or(0);
                    if data.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let request = String::from_utf8_lossy(&data).into_owned();
            assert!(request.starts_with(response.path), "{request}");
            requests.push(request);
            write!(socket, "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.status, response.body.len()).unwrap();
            socket.write_all(&response.body).unwrap();
        }
        requests
    });
    (Settings { server_url: format!("http://{address}"), ..Default::default() }, handle)
}
#[test]
fn asynchronous_command_mock_transport_and_stale_acceptance_work_together() {
    let image = photocraft_codecs::Image::from_u8(8, 8, photocraft_codecs::ChannelLayout::Rgb, vec![127; 8 * 8 * 3]).unwrap();
    let mut script = health();
    script.extend([
        response("POST /api/v1/queue/default/enqueue_batch", json!({"enqueued":1,"item_ids":[7]})),
        response("GET /api/v1/queue/default/i/7", json!({"status":"completed","session":{"results":{"out":{"image":{"image_name":"result.png"}}}}})),
        Response { path: "GET /api/v1/images/i/result.png/full", status: 200, body: images::encode_png(&image).unwrap() },
    ]);
    let (mut settings, handle) = server(script);
    settings.workflows.push(crate::ai_cmds::tests::workflow());
    let mut session = crate::Session::new();
    session.add_document(
        photocraft_doc::Document::new("test", photocraft_doc::Size::new(32, 32), photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U16),
        None,
    );
    session.execute("ai.configure", json!({"settings":settings})).unwrap();
    session.execute("ai.generate", json!({"prompt":"test","width":8,"height":8})).unwrap();
    assert!(session.ai.status.running);
    // Editing remains available while the worker owns its original immutable request.
    session
        .edit("Change selection", |doc, _| {
            let mut mask = photocraft_raster::Surface::new(photocraft_color::PixelFormat::GRAY8);
            mask.write_pixel(5, 5, &[1.0]);
            doc.selection = Some(mask);
            Ok(())
        })
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while session.ai.status.running && std::time::Instant::now() < deadline {
        session.ai.tick();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!session.ai.status.running, "{}", session.ai.status.message);
    assert_eq!(session.ai.candidates.len(), 1, "{}", session.ai.status.message);
    assert_eq!(session.active().unwrap().doc.layers.len(), 0);
    assert!(session.ai.candidates[0].placement.mask.is_none());
    let id = session.ai.candidates[0].id;
    assert!(session.execute("ai.accept", json!({"id":id})).is_err());
    session.execute("ai.accept", json!({"id":id,"allowStale":true})).unwrap();
    assert!(session.active().unwrap().doc.layers[0].mask.is_none());
    assert!(session.undo());
    assert_eq!(session.active().unwrap().doc.layers.len(), 0);
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 6);
    assert!(!requests.iter().any(|r| r.starts_with("POST /api/v1/images/upload")));
    assert!(requests[3].contains("\"runs\":1"));
}
#[test]
fn transport_submission_result_auth_upload_and_cancel() {
    let mut script = health();
    script.extend([
        response("POST /api/v1/images/upload", json!({"image_name":"ref.png"})),
        response("POST /api/v1/queue/default/enqueue_batch", json!({"enqueued":1,"item_ids":[7]})),
        response("GET /api/v1/queue/default/i/7", json!({"status":"completed","session":{"results":{"out":{"image":{"image_name":"result.png"}}}}})),
        Response { path: "GET /api/v1/images/i/result.png/full", status: 200, body: vec![1, 2, 3] },
        response("PUT /api/v1/queue/default/i/7/cancel", json!({"status":"canceled"})),
    ]);
    let (settings, handle) = server(script);
    let mut client = InvokeClient::new(&settings, "token".into()).unwrap();
    assert!(client.health().unwrap().contains("mock"));
    client.validate_graph(&crate::ai_cmds::tests::workflow()).unwrap();
    assert_eq!(client.upload(vec![4, 5, 6], false).unwrap(), "ref.png");
    let job = client.submit(json!({"nodes":{},"edges":[]})).unwrap();
    assert_eq!(job, 7);
    assert!(matches!(client.poll(job, "out").unwrap(), JobStatus::Complete(_)));
    assert_eq!(client.image("result.png").unwrap(), vec![1, 2, 3]);
    client.cancel(job).unwrap();
    let requests = handle.join().unwrap();
    assert!(requests.iter().all(|r| r.to_lowercase().contains("authorization: bearer token")));
    assert!(requests[3].contains("multipart/form-data"));
    assert!(requests[4].contains("\"runs\":1"));
    assert!(client.image("../escape").is_err());
}
#[test]
fn http_failures_bad_json_and_unknown_nodes_are_actionable() {
    for code in [401, 404, 422, 500] {
        let (settings, handle) = server(vec![Response { path: "GET /openapi.json", status: code, body: vec![] }]);
        let error = InvokeClient::new(&settings, String::new()).unwrap().health().unwrap_err().to_string();
        assert!(error.contains(&code.to_string()));
        handle.join().unwrap();
    }
    let (settings, handle) = server(vec![Response { path: "GET /openapi.json", status: 200, body: b"bad json".to_vec() }]);
    assert!(InvokeClient::new(&settings, String::new()).unwrap().health().is_err());
    handle.join().unwrap();
    let (settings, handle) = server(health());
    let mut client = InvokeClient::new(&settings, String::new()).unwrap();
    client.health().unwrap();
    let mut workflow = crate::ai_cmds::tests::workflow();
    workflow.graph["nodes"]["out"]["type"] = json!("invented_flux2_node");
    assert!(client.validate_graph(&workflow).is_err());
    handle.join().unwrap();
}
#[test]
fn connection_failure_and_request_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let settings = Settings { server_url: format!("http://{address}"), request_timeout_secs: 1, ..Default::default() };
    assert!(InvokeClient::new(&settings, String::new()).unwrap().health().is_err());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let settings = Settings { server_url: format!("http://{}", listener.local_addr().unwrap()), request_timeout_secs: 1, ..Default::default() };
    let handle = std::thread::spawn(move || {
        let (_socket, _) = listener.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1200));
    });
    assert!(InvokeClient::new(&settings, String::new()).unwrap().health().unwrap_err().to_string().contains("timed out"));
    handle.join().unwrap();
}
#[test]
#[ignore = "opt-in read-only live InvokeAI connection test"]
fn live_invoke_health() {
    let settings = Settings { server_url: std::env::var("PHOTOCRAFT_INVOKE_URL").unwrap_or_else(|_| Settings::default().server_url), ..Default::default() };
    let mut client = InvokeClient::new(&settings, std::env::var("PHOTOCRAFT_INVOKE_TOKEN").unwrap_or_default()).unwrap();
    println!("{}", client.health().unwrap());
}
