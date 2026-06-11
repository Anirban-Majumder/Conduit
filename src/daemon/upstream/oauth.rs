//! OAuth loopback callback handling.
//!
//! Binds a localhost listener, captures the browser redirect, and returns
//! `(code, state)`. The protocol itself (discovery, DCR, PKCE, token exchange
//! and refresh) lives in rmcp's `transport::auth`; this module only owns the
//! local HTTP redirect catch.

use std::collections::HashMap;
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum OauthError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("listener never received callback within {0:?}")]
    CallbackTimeout(Duration),
    #[error("authorization callback contained no code")]
    MissingCode,
    #[error("cancelled by user")]
    Cancelled,
}

/// Bind `127.0.0.1:0`; returns `(listener, port, redirect_uri)`.
pub async fn bind_callback_listener() -> Result<(TcpListener, u16, String), OauthError> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    Ok((listener, port, redirect_uri))
}

/// Timeout for reading + responding to a single accepted connection.
const PER_CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum bytes read for the HTTP request line; prevents unbounded allocation
/// from a connection that trickle-feeds bytes.
const MAX_REQUEST_LINE_BYTES: usize = 8192;

/// Accept connections until one yields a valid `/callback?code=…` request, reply
/// with HTML, then return `(code, state)`. rmcp validates the CSRF `state` during
/// token exchange, so we don't check it here.
///
/// Browser pre-connect requests (which send nothing or garbage) are discarded and
/// the loop continues. The overall 5-minute deadline and cancel token bound the
/// whole loop; each accepted connection also has a short per-connection timeout so
/// a stalled socket can't park the login forever.
pub async fn wait_for_callback(
    listener: TcpListener,
    cancel: CancellationToken,
) -> Result<(String, String), OauthError> {
    let deadline = Duration::from_secs(300);
    let deadline_at = tokio::time::Instant::now() + deadline;

    loop {
        // Wait for the next connection within the overall deadline.
        let accepted = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(OauthError::Cancelled),
            r = tokio::time::timeout_at(deadline_at, listener.accept()) => {
                r.map_err(|_elapsed| OauthError::CallbackTimeout(deadline))?
            }
        };
        let (mut stream, _addr) = accepted?;

        // Per-connection short timeout covers read_line + write_all.
        // Cancel-safe: the whole block is wrapped in timeout; we don't hold any
        // state across the await points that would be lost on cancellation.
        let conn_result = tokio::time::timeout(PER_CONNECTION_TIMEOUT, async {
            let (rd, mut wr) = stream.split();
            let mut reader = BufReader::new(rd);
            let mut request_line = String::new();

            // Bounded read: stop at the first newline OR the byte cap.
            loop {
                if request_line.len() >= MAX_REQUEST_LINE_BYTES {
                    break;
                }
                let prev_len = request_line.len();
                reader.read_line(&mut request_line).await?;
                // read_line appended nothing → EOF before newline
                if request_line.len() == prev_len {
                    break;
                }
                if request_line.ends_with('\n') {
                    break;
                }
            }

            let query = parse_callback_query(&request_line);

            // Only accept connections that look like the real callback.
            let is_callback = request_line.contains("/callback?");
            let code = query
                .as_ref()
                .and_then(|q| q.get("code"))
                .cloned()
                .unwrap_or_default();
            let state = query
                .as_ref()
                .and_then(|q| q.get("state"))
                .cloned()
                .unwrap_or_default();

            let (status, body) = if !is_callback || code.is_empty() {
                (
                    "400 Bad Request",
                    "<h1>Sign-in failed</h1><p>No authorization code returned. You can close this tab.</p>",
                )
            } else {
                (
                    "200 OK",
                    "<h1>Signed in</h1><p>You can close this tab and return to Conduit.</p>",
                )
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            wr.write_all(response.as_bytes()).await.ok();
            wr.flush().await.ok();

            Ok::<_, std::io::Error>((is_callback, code, state))
        })
        .await;

        match conn_result {
            Err(_elapsed) => {
                // Connection timed out (browser pre-connect / stalled client);
                // fall through to the next accept iteration.
                tracing::debug!(
                    "oauth callback: per-connection timeout; waiting for next connection"
                );
            }
            Ok(Err(e)) => return Err(OauthError::Io(e)),
            Ok(Ok((false, _, _))) => {
                // Not a callback request (e.g. prefetch / favicon); keep waiting.
            }
            Ok(Ok((true, code, _))) if code.is_empty() => return Err(OauthError::MissingCode),
            Ok(Ok((true, code, state))) => return Ok((code, state)),
        }
    }
}

fn parse_callback_query(request_line: &str) -> Option<HashMap<String, String>> {
    let mut parts = request_line.split_whitespace();
    let _method = parts.next()?;
    let path = parts.next()?;
    let query = path.split_once('?')?.1;
    let mut out = HashMap::new();
    for kv in query.split('&') {
        let Some((k, v)) = kv.split_once('=') else {
            continue;
        };
        out.insert(url_decode(k), url_decode(v));
    }
    Some(out)
}

fn url_decode(s: &str) -> String {
    // Collect decoded bytes first, then interpret as UTF-8. Decoding each
    // percent-encoded byte directly as `char` (Latin-1) mangles multibyte
    // sequences — e.g. %C3%A9 ("é") must decode to [0xC3, 0xA9] together.
    let mut bytes: Vec<u8> = Vec::with_capacity(s.len());
    let mut iter = s.bytes();
    while let Some(b) = iter.next() {
        match b {
            b'+' => bytes.push(b' '),
            b'%' => {
                let hi = iter.next().and_then(|c| (c as char).to_digit(16));
                let lo = iter.next().and_then(|c| (c as char).to_digit(16));
                if let (Some(h), Some(l)) = (hi, lo) {
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "h*16+l fits in u8 (max 0xFF)"
                    )]
                    bytes.push((h * 16 + l) as u8);
                } else {
                    bytes.push(b);
                }
            }
            other => bytes.push(other),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_callback_query_extracts_code_and_state() {
        let line = "GET /callback?code=abc123&state=xyz789 HTTP/1.1";
        let q = parse_callback_query(line).expect("parsed");
        assert_eq!(q.get("code").map(String::as_str), Some("abc123"));
        assert_eq!(q.get("state").map(String::as_str), Some("xyz789"));
    }

    #[test]
    fn parse_callback_query_url_decodes_values() {
        let line = "GET /callback?code=a%2Fb&state=x+y HTTP/1.1";
        let q = parse_callback_query(line).expect("parsed");
        assert_eq!(q.get("code").map(String::as_str), Some("a/b"));
        assert_eq!(q.get("state").map(String::as_str), Some("x y"));
    }

    #[test]
    fn parse_callback_query_none_without_query() {
        assert!(parse_callback_query("GET /callback HTTP/1.1").is_none());
    }

    #[test]
    fn url_decode_multibyte_utf8() {
        // "é" = U+00E9 = 0xC3 0xA9 in UTF-8.
        // Latin-1 decoding would yield two garbage chars; correct decoding yields "é".
        assert_eq!(url_decode("%C3%A9"), "é");
        // Three-byte sequence: "€" = U+20AC = 0xE2 0x82 0xAC.
        assert_eq!(url_decode("%E2%82%AC"), "€");
        // Mixed: ASCII + multibyte.
        assert_eq!(url_decode("caf%C3%A9"), "café");
    }
}
