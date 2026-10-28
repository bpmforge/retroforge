//! A minimal HTTP/1.1 client for loopback servers only (ticket W40-02).
//!
//! The local naming backend ([`crate::local`]) talks to a server on this
//! machine (LM Studio, llama.cpp, Ollama's OpenAI endpoint). That needs no
//! TLS and no HTTP client crate: one `POST`, `Connection: close`, read to the
//! end. [`LoopbackTransport`] refuses to connect to anything that is not a
//! loopback address, so selecting the local backend can never send behaviour
//! facts off the machine, whatever URL a settings file holds.
//!
//! Every loop here advances on every path (Law 8): the read loop stops at
//! end of stream, at an error, or at [`MAX_REPLY_BYTES`]; the chunk decoder
//! moves its cursor past each chunk or returns an error.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::anthropic::{HttpReply, Transport};

/// A reply bigger than this is refused (a naming answer is a few KiB).
pub const MAX_REPLY_BYTES: usize = 16 * 1024 * 1024;

/// A parsed `http://host[:port]/path` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpUrl {
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// Split an `http://` URL. `https` is refused: this client is loopback-only
/// and has no TLS.
///
/// # Errors
/// A sentence saying what is wrong with the URL.
pub fn parse_http_url(url: &str) -> Result<HttpUrl, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| "only http:// URLs are supported for the local backend".to_string())?;
    let (authority, path) = rest.find('/').map_or((rest, "/"), |i| rest.split_at(i));
    if authority.is_empty() || authority.contains('@') {
        return Err("the URL has no host (or carries credentials)".to_string());
    }
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (h, after) = v6
            .split_once(']')
            .ok_or_else(|| "unterminated [ in the URL host".to_string())?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse::<u16>().map_err(|_| "bad port".to_string())?,
            None if after.is_empty() => 80,
            None => return Err("junk after the URL host".to_string()),
        };
        (h.to_string(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                p.parse::<u16>().map_err(|_| "bad port".to_string())?,
            ),
            None => (authority.to_string(), 80),
        }
    };
    if host.is_empty() || path.contains(['\r', '\n', ' ']) {
        return Err("the URL has an empty host or illegal characters".to_string());
    }
    Ok(HttpUrl {
        host,
        port,
        path: path.to_string(),
    })
}

/// Whether `url` names this machine: `localhost` or a loopback IP literal.
/// Name resolution is not consulted here ([`LoopbackTransport`] re-checks the
/// resolved addresses at connect time).
#[must_use]
pub fn is_loopback_url(url: &str) -> bool {
    parse_http_url(url).is_ok_and(|u| {
        u.host.eq_ignore_ascii_case("localhost")
            || u.host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

/// A [`Transport`] that only ever connects to loopback addresses.
#[derive(Debug, Clone)]
pub struct LoopbackTransport {
    pub connect_timeout: Duration,
    /// Per read; a local 8B model can think for minutes before the first byte.
    pub read_timeout: Duration,
}

impl Default for LoopbackTransport {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(3),
            read_timeout: Duration::from_secs(600),
        }
    }
}

impl Transport for LoopbackTransport {
    fn post(
        &self,
        url: &str,
        headers: &[(&'static str, String)],
        body: &str,
    ) -> Result<HttpReply, String> {
        let u = parse_http_url(url)?;
        let addrs: Vec<SocketAddr> = (u.host.as_str(), u.port)
            .to_socket_addrs()
            .map_err(|e| format!("cannot resolve {}: {e}", u.host))?
            .collect();
        if addrs.is_empty() || addrs.iter().any(|a| !a.ip().is_loopback()) {
            return Err(format!("{} is not a loopback address", u.host));
        }
        let mut last = "no address to try".to_string();
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, self.connect_timeout) {
                Ok(mut s) => {
                    s.set_read_timeout(Some(self.read_timeout))
                        .and_then(|()| s.set_write_timeout(Some(Duration::from_secs(30))))
                        .map_err(|e| e.to_string())?;
                    return exchange(&mut s, &u, headers, body);
                }
                Err(e) => last = format!("cannot connect to {addr}: {e}"),
            }
        }
        Err(last)
    }
}

fn exchange(
    s: &mut TcpStream,
    u: &HttpUrl,
    headers: &[(&'static str, String)],
    body: &str,
) -> Result<HttpReply, String> {
    let mut req = format!(
        "POST {} HTTP/1.1\r\nHost: {}:{}\r\n",
        u.path, u.host, u.port
    );
    for (k, v) in headers {
        if v.contains(['\r', '\n']) {
            return Err(format!("header `{k}` has a line break"));
        }
        let lower = k.to_ascii_lowercase();
        if matches!(lower.as_str(), "host" | "content-length" | "connection") {
            continue;
        }
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    s.write_all(req.as_bytes())
        .and_then(|()| s.write_all(body.as_bytes()))
        .and_then(|()| s.flush())
        .map_err(|e| format!("write failed: {e}"))?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > MAX_REPLY_BYTES {
                    return Err("the reply is larger than the limit".to_string());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("read failed: {e}")),
        }
    }
    parse_reply(&raw)
}

/// Parse a whole `Connection: close` HTTP/1.1 reply.
///
/// # Errors
/// A sentence for a malformed reply.
pub fn parse_reply(raw: &[u8]) -> Result<HttpReply, String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "no end of headers in the reply".to_string())?;
    let head = std::str::from_utf8(&raw[..split]).map_err(|_| "non-UTF-8 headers".to_string())?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| "bad status line".to_string())?;
    let mut chunked = false;
    let mut retry_after = None;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            } else if k == "retry-after" {
                retry_after = v.parse::<u64>().ok();
            }
        }
    }
    let payload = &raw[split + 4..];
    let bytes = if chunked {
        dechunk(payload)?
    } else {
        payload.to_vec()
    };
    let body = String::from_utf8(bytes).map_err(|_| "the body is not UTF-8".to_string())?;
    Ok(HttpReply {
        status,
        body,
        retry_after,
    })
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let eol = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| "truncated chunk header".to_string())?;
        let size_text = std::str::from_utf8(&data[..eol]).map_err(|_| "bad chunk header")?;
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| "bad chunk size")?;
        data = &data[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        // Each pass consumes `eol + 2 + size + 2 > 0` bytes or returns.
        if data.len() < size + 2 || out.len() + size > MAX_REPLY_BYTES {
            return Err("truncated or oversized chunk".to_string());
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}
