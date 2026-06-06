//! E2: multi-pane web SSH terminal — a WebSocket ⇄ PTY bridge.
//!
//! HUMAN-ADMIN ONLY. The whole `/api/agent/*` surface is gated to the admin
//! session (Admin scope) by the `/api/agent/` deny-list in `auth.rs`, so this
//! interactive shell is never reachable by an agent token or MCP. The browser
//! opens `wss://<host>/api/agent/systems/<id>/terminal/ws`; the same-origin
//! `aeon_session` cookie rides along and authenticates as the admin (exactly
//! like the streamer's H.264 WebSocket).
//!
//! On connect we look up the registered system, then spawn the agent-connect
//! `ssh` inside a real PTY (portable-pty) — or, for the special id `local`, a
//! login shell on the Orb itself (`su -l admin`, no ssh) — so full-screen
//! TUIs (vim/htop),
//! 256-colour, window resize and ctrl-keys all work end-to-end:
//!
//!   ssh -i <agent-connect key> -tt -o BatchMode=yes \
//!       -o StrictHostKeyChecking=accept-new \
//!       -o PreferredAuthentications=publickey -p <port> <user>@<addr>
//!
//! Bridge:
//!   • PTY master reader  → WS binary frames   (blocking read on a thread →
//!                                               tokio mpsc → WS writer task)
//!   • WS frames          → PTY master writer  (text JSON `{"type":"resize"…}`
//!                                               resizes the PTY; everything
//!                                               else is raw stdin bytes)
//! On WS close we kill the ssh child + drop the PTY; on ssh exit we close WS.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use std::io::{Read, Write};

use crate::api::AppState;

/// GET /agent/systems/:id/terminal/ws — upgrade to a WebSocket and bridge it to
/// an interactive SSH PTY against the registered system `:id`.
pub async fn terminal_ws(
    State(_state): State<AppState>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| bridge(socket, id))
}

/// A control frame the client sends to resize the remote terminal.
#[derive(serde::Deserialize)]
struct ResizeMsg {
    #[serde(rename = "type")]
    kind: String,
    cols: u16,
    rows: u16,
}

async fn bridge(mut socket: WebSocket, id: String) {
    // Resolve the target. The special id `local` is the Orb itself — no system
    // lookup, a local login shell instead of ssh. Otherwise look up the
    // registered system's SSH params; if it's gone, tell the client and close.
    let target = if id == "local" {
        None
    } else {
        match crate::agent_connect::ssh_target(&id) {
            Some(t) => Some(t),
            None => {
                let _ = socket
                    .send(Message::Text(format!(
                        "\r\n\x1b[31maeon: no such system '{id}'\x1b[0m\r\n"
                    )))
                    .await;
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
        }
    };

    // ── Open a PTY and spawn the shell inside it ────────────────────────
    let pty_system = NativePtySystem::default();
    let pair = match pty_system.openpty(PtySize {
        rows: 30,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("\r\n\x1b[31maeon: openpty failed: {e}\x1b[0m\r\n")))
                .await;
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };

    let mut cmd = match target {
        // Remote system: agent-connect ssh in a PTY.
        Some(t) => {
            let key = crate::agent_connect::agent_key_path();
            let mut c = CommandBuilder::new("ssh");
            c.arg("-i");
            c.arg(&key);
            c.args([
                "-tt",
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "PreferredAuthentications=publickey",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=4",
                "-p",
                &t.port.to_string(),
            ]);
            c.arg(format!("{}@{}", t.ssh_user, t.address));
            c
        }
        // The Orb itself: a local login shell as the admin user. The supervisor
        // runs as root, so `su -l admin` drops to admin without a password and
        // gives the same shell + environment the admin gets over SSH — no ssh,
        // no key, no network hop. Still admin-gated: the whole /api/agent/*
        // surface is Admin-scope only, never reachable by an agent token/MCP.
        None => {
            let mut c = CommandBuilder::new("su");
            c.args(["-l", "admin"]);
            c
        }
    };
    // A sensible TERM so colour + curses apps behave either way.
    cmd.env("TERM", "xterm-256color");

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("\r\n\x1b[31maeon: shell spawn failed: {e}\x1b[0m\r\n")))
                .await;
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    // Drop the slave handle in the parent: the child holds the only slave fd,
    // so the master read sees EOF as soon as ssh exits.
    drop(pair.slave);

    // Reader (PTY master → bytes) and writer (bytes → PTY master) handles.
    let mut reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("\r\n\x1b[31maeon: pty reader: {e}\x1b[0m\r\n")))
                .await;
            let _ = child.kill();
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    let mut writer = match pair.master.take_writer() {
        Ok(w) => w,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("\r\n\x1b[31maeon: pty writer: {e}\x1b[0m\r\n")))
                .await;
            let _ = child.kill();
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    // Keep the master alive for the whole session (needed for resize); it's
    // moved into the blocking writer thread below so resize uses a clone.
    let master = pair.master;

    // ── PTY master → WS (blocking reads on a thread, forwarded via mpsc) ──
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let reader_task = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,            // EOF: ssh exited / pty closed
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break; // WS side gone
                    }
                }
                Err(_) => break,
            }
        }
    });

    // ── WS stdin → PTY master writer (blocking writes on a thread) ───────
    let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let writer_task = tokio::task::spawn_blocking(move || {
        while let Some(bytes) = in_rx.blocking_recv() {
            if writer.write_all(&bytes).is_err() {
                break;
            }
            let _ = writer.flush();
        }
    });

    // ── Main select loop ────────────────────────────────────────────────
    loop {
        tokio::select! {
            // Bytes from the PTY → binary WS frame.
            maybe = out_rx.recv() => {
                match maybe {
                    Some(chunk) => {
                        if socket.send(Message::Binary(chunk)).await.is_err() {
                            break; // client gone
                        }
                    }
                    None => break, // reader finished (ssh exited)
                }
            }
            // Frames from the client → resize control or raw stdin.
            ws_in = socket.recv() => {
                match ws_in {
                    Some(Ok(Message::Text(txt))) => {
                        // A JSON resize control message? Otherwise raw stdin.
                        if let Ok(r) = serde_json::from_str::<ResizeMsg>(&txt) {
                            if r.kind == "resize" {
                                let _ = master.resize(PtySize {
                                    rows: r.rows.max(1),
                                    cols: r.cols.max(1),
                                    pixel_width: 0,
                                    pixel_height: 0,
                                });
                                continue;
                            }
                        }
                        if in_tx.send(txt.into_bytes()).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(bin))) => {
                        if in_tx.send(bin).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                }
            }
        }
    }

    // ── Teardown: kill ssh, drop channels/threads, close WS ─────────────
    let _ = child.kill();
    let _ = child.wait();
    drop(in_tx);          // ends the writer thread's recv loop
    drop(out_rx);         // unblocks the reader's blocking_send → ends thread
    drop(master);         // close the PTY master
    reader_task.abort();
    writer_task.abort();
    let _ = socket.send(Message::Close(None)).await;
}
