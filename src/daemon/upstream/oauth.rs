//! OAuth loopback callback handling.
//!
//! Binds a localhost listener, captures the browser redirect, and returns
//! `(code, state)`. The protocol itself (discovery, DCR, PKCE, token exchange
//! and refresh) lives in rmcp's `transport::auth`; this module only owns the
//! local HTTP redirect catch and draft-token cleanup.

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

/// Accept one redirect, reply with HTML, return `(code, state)`. rmcp validates
/// the CSRF `state` during token exchange, so we don't check it here. 5-minute
/// hard cap.
pub async fn wait_for_callback(
    listener: TcpListener,
    cancel: CancellationToken,
) -> Result<(String, String), OauthError> {
    let deadline = Duration::from_secs(300);
    let accepted = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(OauthError::Cancelled),
        r = tokio::time::timeout(deadline, listener.accept()) => {
            r.map_err(|_elapsed| OauthError::CallbackTimeout(deadline))?
        }
    };
    let (mut stream, _addr) = accepted?;
    let (rd, mut wr) = stream.split();
    let mut reader = BufReader::new(rd);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let query = parse_callback_query(&request_line).unwrap_or_default();
    let state = query.get("state").cloned().unwrap_or_default();
    let code = query.get("code").cloned().unwrap_or_default();

    let (status, body) = if code.is_empty() {
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

    if code.is_empty() {
        return Err(OauthError::MissingCode);
    }
    Ok((code, state))
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
    let mut out = String::with_capacity(s.len());
    let mut iter = s.bytes();
    while let Some(b) = iter.next() {
        match b {
            b'+' => out.push(' '),
            b'%' => {
                let hi = iter.next().and_then(|c| (c as char).to_digit(16));
                let lo = iter.next().and_then(|c| (c as char).to_digit(16));
                if let (Some(h), Some(l)) = (hi, lo) {
                    let combined = h * 16 + l;
                    let byte = u8::try_from(combined).unwrap_or(b'?');
                    out.push(char::from(byte));
                } else {
                    out.push(b as char);
                }
            }
            other => out.push(other as char),
        }
    }
    out
}

/// Drop draft-session keyring entries left over from abandoned wizards.
pub fn purge_draft_oauth_tokens(known_drafts: &[String]) -> usize {
    let mut count = 0;
    for d in known_drafts {
        if uuid::Uuid::parse_str(d).is_ok()
            && crate::daemon::secrets::delete_oauth_tokens(d).is_ok()
        {
            count += 1;
        }
    }
    count
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
}
