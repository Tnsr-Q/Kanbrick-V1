//! A minimal, dependency-free blocking HTTP/1.1 client for the executor split
//! (#70).
//!
//! The control plane forwards invocations to executor pods, and executors call
//! back to the control plane's internal RPC surface. All of this traffic is
//! **in-cluster, plain HTTP** between ClusterIP Services — never TLS, never
//! through the agent egress proxy. A handful of `GET`/`POST`-with-JSON calls do
//! not justify a heavyweight async HTTP client and its transitive tree, so this
//! is hand-rolled over [`std::net::TcpStream`] (the same minimal-dependency
//! reasoning behind the hand-rolled Prometheus renderer in [`crate::metrics`]).
//!
//! Scope is deliberately narrow: one request per connection (`Connection:
//! close`), `Content-Length`-framed or read-to-EOF bodies. Chunked transfer
//! encoding is rejected — the control-plane/executor handlers always return
//! fixed-size bodies, so they are never chunked.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// A parsed HTTP response: the status code and the raw body bytes.
pub struct HttpResponse {
    /// The HTTP status code (e.g. `200`, `401`, `404`).
    pub status: u16,
    /// The response body bytes (decoded; never chunk-framed).
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Whether the status is in the 2xx success range.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Perform one blocking HTTP/1.1 request. `url` must be `http://host[:port]/path`.
/// `headers` are sent verbatim (a `Host`, `Connection: close`, and
/// `Content-Length` are always added). A `None` body sends `Content-Length: 0`.
pub fn request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    timeout: Duration,
) -> std::io::Result<HttpResponse> {
    let (host, port, path) = parse_url(url)?;
    let sockaddr = (host.as_str(), port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| err(format!("could not resolve {host}:{port}")))?;

    let mut stream = TcpStream::connect_timeout(&sockaddr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    let body = body.unwrap_or(&[]);
    let mut head = String::new();
    head.push_str(&format!("{method} {path} HTTP/1.1\r\n"));
    head.push_str(&format!("Host: {host}\r\n"));
    head.push_str("Connection: close\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    head.push_str("\r\n");

    let mut request = head.into_bytes();
    request.extend_from_slice(body);
    stream.write_all(&request)?;
    stream.flush()?;

    // The server honours `Connection: close`, so reading to EOF yields the whole
    // response. Bodies are `Content-Length`-framed (not chunked), so everything
    // after the header terminator is the body verbatim.
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    parse_response(&raw)
}

/// Split a `http://host[:port]/path` URL into `(host, port, path)`.
fn parse_url(url: &str) -> std::io::Result<(String, u16, String)> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| err(format!("only http:// URLs are supported: {url}")))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| err(format!("invalid port in {url}")))?,
        ),
        None => (authority.to_string(), 80),
    };
    if host.is_empty() {
        return Err(err(format!("missing host in {url}")));
    }
    Ok((host, port, path.to_string()))
}

/// Parse the status line and split off the body. Rejects chunked responses.
fn parse_response(raw: &[u8]) -> std::io::Result<HttpResponse> {
    let sep = find_subsequence(raw, b"\r\n\r\n")
        .ok_or_else(|| err("no header terminator in response".to_string()))?;
    let body = raw[sep + 4..].to_vec();
    let head = String::from_utf8_lossy(&raw[..sep]);

    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| err("empty response".to_string()))?;
    // "HTTP/1.1 200 OK" -> 200
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| err(format!("malformed status line: {status_line}")))?;

    for line in lines {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            return Err(err("chunked transfer-encoding is not supported".to_string()));
        }
    }

    Ok(HttpResponse { status, body })
}

/// Find the first index of `needle` within `haystack`.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Build an [`std::io::Error`] with an `Other` kind from a message.
fn err(message: String) -> std::io::Error {
    std::io::Error::other(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn parse_url_splits_host_port_path() {
        assert_eq!(
            parse_url("http://cp:8090/internal/registry").unwrap(),
            ("cp".to_string(), 8090, "/internal/registry".to_string())
        );
        assert_eq!(
            parse_url("http://127.0.0.1:1/x").unwrap(),
            ("127.0.0.1".to_string(), 1, "/x".to_string())
        );
        // No explicit port defaults to 80; no path defaults to "/".
        assert_eq!(
            parse_url("http://host").unwrap(),
            ("host".to_string(), 80, "/".to_string())
        );
    }

    #[test]
    fn rejects_non_http_and_bad_port() {
        assert!(parse_url("https://x/y").is_err());
        assert!(parse_url("http://h:notaport/").is_err());
    }

    #[test]
    fn parse_response_extracts_status_and_body() {
        let raw = b"HTTP/1.1 404 Not Found\r\ncontent-length: 3\r\n\r\nbad";
        let resp = parse_response(raw).unwrap();
        assert_eq!(resp.status, 404);
        assert!(!resp.is_success());
        assert_eq!(resp.body, b"bad");
    }

    #[test]
    fn parse_response_rejects_chunked() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        assert!(parse_response(raw).is_err());
    }

    #[test]
    fn round_trips_against_a_local_server() {
        // A throwaway one-shot server that echoes a fixed JSON body, proving the
        // client writes a well-formed request and reads back the framed response.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).unwrap();
            let payload = br#"{"ok":true}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            sock.write_all(resp.as_bytes()).unwrap();
            sock.write_all(payload).unwrap();
        });

        let url = format!("http://{addr}/echo");
        let resp = request(
            "POST",
            &url,
            &[("content-type", "application/json")],
            Some(br#"{"q":1}"#),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, br#"{"ok":true}"#);
        handle.join().unwrap();
    }
}
