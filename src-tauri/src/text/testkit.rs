//! Test helpers: a one-shot local HTTP server that records requests and replies with canned
//! responses, so the AI and integration code can be tested without the internet.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// Serves `responses` in order (one connection each, then closes) and returns the base URL plus
/// a handle that yields the raw requests it saw.
pub(crate) async fn mock_server(responses: Vec<(u16, String)>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock server");
    let addr = listener.local_addr().expect("mock server address");
    let handle = tokio::spawn(async move {
        let mut seen = Vec::new();
        for (status, body) in responses {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            seen.push(read_request(&mut sock).await);
            let reply = format!(
                "HTTP/1.1 {status} Mock\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(reply.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
        seen
    });
    (format!("http://{addr}"), handle)
}

/// A local address that refuses connections (bound, then released).
pub(crate) async fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("address");
    drop(listener);
    format!("http://{addr}")
}

async fn read_request(sock: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = match sock.read(&mut tmp).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= pos + 4 + len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

/// The JSON body of a recorded request.
pub(crate) fn body_json(request: &str) -> serde_json::Value {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body).unwrap_or(serde_json::Value::Null)
}
