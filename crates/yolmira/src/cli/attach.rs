//! `yolmira shell attach`: output arrives on one connection (read by its own task, never
//! cancelled mid-line); input and resizes go over a second. The console guard restores
//! the terminal however this ends.

use super::commands::{client_error, connect_started};
use super::console::{window_size, InputReader, RawConsole};
use super::exit;
use super::output::Out;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde_json::json;
use std::io::Write;
use std::time::Duration;
use tokio::sync::mpsc;
use yolmira_core::YolmiraError;
use yolmira_ipc::protocol::{
    methods, notifications, SessionAttachResult, SessionExited, SessionIdParams,
    SessionInputParams, SessionOutput, SessionResizeParams,
};
use yolmira_ipc::{Client, Instance};

/// Ctrl+] detaches (the shell keeps running).
const DETACH_KEY: u8 = 0x1d;
pub const DEFAULT_SIZE: (u16, u16) = (120, 30);
const RESIZE_POLL: Duration = Duration::from_millis(250);

enum Feed {
    Output(Vec<u8>, bool),
    Exited(Option<i64>),
    Closed(String),
}

struct Attached {
    id: String,
    ctl: Client,
    size: (u16, u16),
}

pub async fn attach(out: &Out, instance: Instance, id: String) -> Result<u8, YolmiraError> {
    let console = RawConsole::enable()?;
    let mut ctl = connect_started(&instance).await?;
    let mut feed = Client::connect(&instance, "yolmira-cli-attach")
        .await
        .map_err(client_error)?;
    let _: SessionAttachResult = feed
        .call(
            methods::SESSION_ATTACH,
            json!(SessionIdParams { id: id.clone() }),
        )
        .await
        .map_err(client_error)?;
    let size = window_size().unwrap_or(DEFAULT_SIZE);
    if let Err(e) = resize(&mut ctl, &id, size).await {
        // Not fatal: the shell keeps its current size.
        write_out(format!("[yolmira: resize failed: {}]\r\n", e.why).as_bytes());
    }
    let in_rx = spawn_input(InputReader::new(console.input_handle()));
    let (feed_tx, feed_rx) = mpsc::channel::<Feed>(256);
    tokio::spawn(read_feed(feed, feed_tx));
    let (why, code) = run(
        Attached {
            id: id.clone(),
            ctl,
            size,
        },
        in_rx,
        feed_rx,
    )
    .await;
    drop(console); // restore the terminal before printing
    out.value(
        || format!("\r\n[yolmira: session {id} {why}]"),
        json!({ "id": id, "ended": why }),
    );
    Ok(code)
}

/// The attach loop. Every way out returns a reason; nothing here skips the restore.
async fn run(
    mut a: Attached,
    mut input: mpsc::Receiver<Vec<u8>>,
    mut feed: mpsc::Receiver<Feed>,
) -> (String, u8) {
    let mut tick = tokio::time::interval(RESIZE_POLL);
    loop {
        tokio::select! {
            Some(bytes) = input.recv() => {
                let detach = find_detach(&bytes);
                let send = &bytes[..detach.unwrap_or(bytes.len())];
                if !send.is_empty() {
                    if let Err(e) = send_input(&mut a.ctl, &a.id, send).await {
                        return (format!("ended: {}", e.why), exit::OK);
                    }
                }
                if detach.is_some() {
                    return ("detached; it keeps running".into(), exit::OK);
                }
            }
            Some(msg) = feed.recv() => match msg {
                Feed::Output(data, reset) => {
                    if reset {
                        write_out(b"\x1b[2J\x1b[H");
                    }
                    write_out(&data);
                }
                Feed::Exited(code) => return (format!("exited with code {}", code.map_or("?".into(), |c| c.to_string())), exit::OK),
                Feed::Closed(why) => return (format!("connection closed: {why}"), exit::ERROR),
            },
            _ = tick.tick() => {
                if let Some(now) = window_size().filter(|s| *s != a.size) {
                    a.size = now;
                    if let Err(e) = resize(&mut a.ctl, &a.id, now).await {
                        return (format!("ended: {}", e.why), exit::OK);
                    }
                }
            }
        }
    }
}

fn spawn_input(mut reader: InputReader) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
    // Blocks in ReadConsoleW; it ends with the process after the attach returns.
    std::thread::spawn(move || loop {
        match reader.read() {
            Ok(Some(bytes)) if bytes.is_empty() => continue,
            Ok(Some(bytes)) => {
                if tx.blocking_send(bytes).is_err() {
                    return;
                }
            }
            Ok(None) | Err(_) => return,
        }
    });
    rx
}

/// Where the detach key starts in `input`: the raw byte, or a win32-input-mode key-down
/// (`ESC [ Vk ; Sc ; Uc ; Kd ; Cs ; Rc _` with Uc = 29, Kd = 1). The remote ConPTY asks
/// for win32-input-mode, and its request reaches this terminal through the output.
pub fn find_detach(input: &[u8]) -> Option<usize> {
    let raw = input.iter().position(|&b| b == DETACH_KEY);
    let mut i = 0;
    while i + 1 < input.len() {
        if input[i] == 0x1b && input[i + 1] == b'[' {
            let start = i + 2;
            if let Some(len) = input[start..]
                .iter()
                .position(|&b| !(b.is_ascii_digit() || b == b';'))
            {
                let end = start + len;
                if input[end] == b'_' && is_detach_key_down(&input[start..end]) {
                    return Some(raw.map_or(i, |r| r.min(i)));
                }
                i = end;
            }
        }
        i += 1;
    }
    raw
}

fn is_detach_key_down(params: &[u8]) -> bool {
    let p: Vec<u32> = std::str::from_utf8(params)
        .unwrap_or("")
        .split(';')
        .map(|x| x.parse().unwrap_or(0))
        .collect();
    p.get(2) == Some(&u32::from(DETACH_KEY)) && p.get(3) == Some(&1)
}

async fn read_feed(mut feed: Client, tx: mpsc::Sender<Feed>) {
    loop {
        let msg = match feed.next_notification().await {
            Ok(n) if n.method == notifications::SESSION_OUTPUT => {
                match serde_json::from_value::<SessionOutput>(n.params)
                    .map_err(|e| e.to_string())
                    .and_then(|o| {
                        B64.decode(o.data)
                            .map(|d| (d, o.reset))
                            .map_err(|e| e.to_string())
                    }) {
                    Ok((data, reset)) => Feed::Output(data, reset),
                    Err(e) => Feed::Closed(format!("bad output from the daemon: {e}")),
                }
            }
            Ok(n) if n.method == notifications::SESSION_EXITED => Feed::Exited(
                serde_json::from_value::<SessionExited>(n.params)
                    .ok()
                    .and_then(|e| e.exit_code),
            ),
            Ok(_) => continue,
            Err(e) => Feed::Closed(e.to_string()),
        };
        let last = !matches!(msg, Feed::Output(..));
        if tx.send(msg).await.is_err() || last {
            return;
        }
    }
}

async fn send_input(ctl: &mut Client, id: &str, bytes: &[u8]) -> Result<(), YolmiraError> {
    let params = SessionInputParams {
        id: id.into(),
        data: B64.encode(bytes),
    };
    ctl.call_value(methods::SESSION_INPUT, json!(params))
        .await
        .map(|_| ())
        .map_err(client_error)
}

async fn resize(ctl: &mut Client, id: &str, (cols, rows): (u16, u16)) -> Result<(), YolmiraError> {
    let params = SessionResizeParams {
        id: id.into(),
        cols,
        rows,
    };
    ctl.call_value(methods::SESSION_RESIZE, json!(params))
        .await
        .map(|_| ())
        .map_err(client_error)
}

fn write_out(bytes: &[u8]) {
    let mut stdout = std::io::stdout().lock();
    // A closed console ends the attach through the input side; nothing to report here.
    let _ = stdout.write_all(bytes).and_then(|_| stdout.flush());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_is_found_in_both_encodings() {
        assert_eq!(find_detach(b"ab\x1dc"), Some(2));
        // Ctrl+] key down in win32-input-mode (VK_OEM_6 = 221, scan 27, char 29).
        assert_eq!(
            find_detach(b"x\x1b[221;27;29;1;8;1_\x1b[221;27;29;0;8;1_"),
            Some(1)
        );
        // Key up alone, other keys, and plain CSI sequences don't detach.
        assert_eq!(find_detach(b"\x1b[221;27;29;0;8;1_"), None);
        assert_eq!(find_detach(b"\x1b[65;30;97;1;0;1_\x1b[2J"), None);
        assert_eq!(find_detach(b"plain"), None);
    }
}
