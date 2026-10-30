//! The transport against a one-shot local server: what actually goes out.

use collection::model::{Auth, KeyValue, RequestDef};
use collection::send::{send, SendOptions, Variables};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};

/// Accepts one request, answers 404, and returns the request's header lines.
fn serve_once() -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = std::io::BufReader::new(stream);
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            headers.push(line.trim_end().to_ascii_lowercase());
        }
        reader
            .get_mut()
            .write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\nConnection: close\r\n\r\nno",
            )
            .unwrap();
        headers
    });
    (url, server)
}

#[test]
fn auth_replaces_a_hand_written_header_and_a_404_is_a_response() {
    let (url, server) = serve_once();
    let request = RequestDef {
        method: "GET".into(),
        url,
        headers: vec![KeyValue {
            name: "Authorization".into(),
            value: "Bearer stale".into(),
            disabled: false,
        }],
        ..Default::default()
    };
    let auth = Auth {
        kind: "bearer".into(),
        fields: [("token".to_string(), "fresh".to_string())].into(),
    };

    let sent = send(
        &request,
        Some(&auth),
        &Variables::from(BTreeMap::new()),
        &SendOptions::default(),
    )
    .unwrap();

    assert_eq!(sent.status, 404);
    assert_eq!(sent.body, b"no");
    let authorization: Vec<_> = server
        .join()
        .unwrap()
        .into_iter()
        .filter(|line| line.starts_with("authorization:"))
        .collect();
    assert_eq!(authorization, ["authorization: bearer fresh"]);
}
