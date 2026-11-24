//! Minimal RTSP 1.0 (RFC 2326) message parsing / serialization.
//!
//! Only what a RAVENNA RTSP server and its clients actually need: a request line,
//! case-insensitive headers, an optional application/sdp body, and CSeq echoes.

use anyhow::{bail, Result};

pub const RTSP_VERSION: &str = "RTSP/1.0";

#[derive(Debug, Clone)]
pub struct RtspRequest {
    pub method: String,
    pub uri: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RtspRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn cseq(&self) -> Option<&str> {
        self.header("cseq")
    }

    /// True when the first token is a status line (RTSP/1.0 200 OK): that is a
    /// response, e.g. the client acknowledging a server-initiated ANNOUNCE.
    pub fn is_response(&self) -> bool {
        self.method.starts_with("RTSP/")
    }

    /// Path part of the request URI, percent-decoded.
    pub fn path(&self) -> String {
        extract_path(&self.uri)
    }
}

pub fn extract_path(uri: &str) -> String {
    if uri == "*" || uri.is_empty() {
        return uri.to_string();
    }
    let after_scheme = uri.split_once("://").map(|(_, rest)| rest).unwrap_or(uri);
    let path = match after_scheme.find('/') {
        Some(i) => &after_scheme[i..],
        None => "/",
    };
    let path = path.split(['?', '#']).next().unwrap_or("/");
    percent_decode(path)
}

/// RFC 3986 percent decoding (bytes, interpreted as UTF-8 where possible).
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn find_header_end(buf: &[u8]) -> Option<(usize, usize)> {
    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some((i, i + 4));
    }
    buf.windows(2)
        .position(|w| w == b"\n\n")
        .map(|i| (i, i + 2))
}

/// Parse one request from buf.
///
/// Returns Ok(None) when the buffer does not yet hold a complete message, and
/// the number of consumed bytes otherwise.
pub fn parse_request(buf: &[u8]) -> Result<Option<(RtspRequest, usize)>> {
    let Some((head_end, body_start)) = find_header_end(buf) else {
        return Ok(None);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]);
    let mut lines = head.lines();
    let start = lines.next().unwrap_or_default().trim_end();
    let mut parts = start.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let uri = parts.next().unwrap_or("/").to_string();
    let version = parts.next().unwrap_or(RTSP_VERSION).to_string();
    if method.is_empty() {
        bail!("malformed RTSP start line: {start:?}");
    }
    let mut headers = Vec::new();
    for line in lines {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let content_length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let total = body_start + content_length;
    if buf.len() < total {
        return Ok(None);
    }
    let body = buf[body_start..total].to_vec();
    Ok(Some((
        RtspRequest {
            method,
            uri,
            version,
            headers,
            body,
        },
        total,
    )))
}

#[derive(Debug, Clone)]
pub struct RtspResponse {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RtspResponse {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            reason: default_reason(status).to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn ok() -> Self {
        Self::new(200)
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn with_cseq(mut self, cseq: Option<&str>) -> Self {
        if let Some(cseq) = cseq {
            self.headers.push(("CSeq".to_string(), cseq.to_string()));
        }
        self
    }

    pub fn with_body(mut self, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        self.headers
            .push(("Content-Type".to_string(), content_type.to_string()));
        self.body = body.into();
        self
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut head = format!("{RTSP_VERSION} {} {}\r\n", self.status, self.reason);
        for (k, v) in &self.headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        head.push_str("\r\n");
        let mut out = head.into_bytes();
        out.extend_from_slice(&self.body);
        out
    }
}

fn default_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        454 => "Session Not Found",
        461 => "Unsupported Transport",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "OK",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_describe_with_cseq() {
        let raw = b"DESCRIBE rtsp://192.168.1.10:8554/by-name/Line%201 RTSP/1.0\r\nCSeq: 312\r\nAccept: application/sdp\r\n\r\n";
        let (req, consumed) = parse_request(raw).unwrap().unwrap();
        assert_eq!(consumed, raw.len());
        assert_eq!(req.method, "DESCRIBE");
        assert_eq!(req.cseq(), Some("312"));
        assert_eq!(req.path(), "/by-name/Line 1");
    }

    #[test]
    fn waits_for_incomplete_body() {
        let raw =
            b"ANNOUNCE rtsp://x/ RTSP/1.0\r\nCSeq: 1\r\nContent-Length: 10\r\n\r\nv=0";
        assert!(parse_request(raw).unwrap().is_none());
    }

    #[test]
    fn extracts_paths() {
        assert_eq!(extract_path("rtsp://h:8554/by-id/1"), "/by-id/1");
        assert_eq!(extract_path("/by-name/x"), "/by-name/x");
        assert_eq!(extract_path("*"), "*");
        assert_eq!(extract_path("rtsp://h:8554"), "/");
    }
}
