//! The first request on a connection must be `hello` with the current token.

use super::server::{send, to_line};
use super::state::State;
use amiri_ipc::framing::{read_line, MAX_HELLO_LINE};
use amiri_ipc::protocol::{
    codes, methods, HelloParams, HelloResult, Request, Response, RpcError, PROTOCOL_VERSION,
};
use serde_json::json;
use std::time::Duration;
use tokio::io::AsyncBufRead;
use tokio::sync::mpsc;

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CLIENT_NAME: usize = 64;

/// Returns the client name on success. Failures are answered, recorded (rate-limited)
/// and end the connection.
pub async fn authenticate<R: AsyncBufRead + Unpin>(
    state: &State,
    reader: &mut R,
    tx: &mpsc::Sender<String>,
) -> Option<(String, u64)> {
    let line = match tokio::time::timeout(HELLO_TIMEOUT, read_line(reader, MAX_HELLO_LINE)).await {
        Ok(Ok(Some(line))) => line,
        Ok(Ok(None)) => return None, // closed before speaking: a probe, not an attack
        Ok(Err(_)) => return reject(state, tx, None, "bad_frame", None).await,
        Err(_) => return reject(state, tx, None, "timeout", None).await,
    };
    let Ok(req) = serde_json::from_str::<Request>(&line) else {
        return reject(state, tx, None, "not_json", None).await;
    };
    if req.method != methods::HELLO {
        return reject(state, tx, Some(req.id), "no_hello", None).await;
    }
    let Ok(hello) = serde_json::from_value::<HelloParams>(req.params.clone()) else {
        return reject(state, tx, Some(req.id), "bad_hello", None).await;
    };
    if !valid_client_name(&hello.client) {
        return reject(state, tx, Some(req.id), "bad_client_name", None).await;
    }
    if !constant_time_eq(hello.token.as_bytes(), state.token.as_bytes()) {
        return reject(state, tx, Some(req.id), "bad_token", Some(&hello.client)).await;
    }
    if hello.protocol_version != PROTOCOL_VERSION {
        let msg = format!(
            "protocol {} not supported; daemon speaks {PROTOCOL_VERSION}",
            hello.protocol_version
        );
        send(
            tx,
            &Response::err(req.id, RpcError::new(codes::INVALID_REQUEST, msg)),
        )
        .await;
        return None;
    }
    let connection = state.new_connection_id();
    let result = HelloResult {
        daemon_version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
        pid: std::process::id(),
        instance: state.instance.name.clone(),
        connection,
    };
    send(tx, &Response::ok(req.id, json!(result))).await;
    Some((hello.client, connection))
}

/// Client names end up in events and terminal output: short printable ASCII only.
pub fn valid_client_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_CLIENT_NAME && name.chars().all(|c| c.is_ascii_graphic())
}

async fn reject<T>(
    state: &State,
    tx: &mpsc::Sender<String>,
    id: Option<u64>,
    reason: &str,
    client: Option<&str>,
) -> Option<T> {
    state.record_auth_failure(reason, client);
    let error = RpcError::new(codes::UNAUTHORIZED, "unauthorized");
    let response = match id {
        Some(id) => Response::err(id, error),
        None => Response::err_unidentified(error),
    };
    // Best effort: the peer may already be gone.
    let _ = tx.send(to_line(&response)).await;
    None
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_names_are_short_printable_ascii() {
        assert!(valid_client_name("amiri-cli"));
        for bad in ["", "a b", "\u{1b}[31m", "имя", &"x".repeat(65)] {
            assert!(!valid_client_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn token_compare() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
