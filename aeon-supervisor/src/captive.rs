//! HTTP-only listener on port 80 for the AP-setup captive portal flow.
//!
//! When the Pi is running its `aeon-setup` fallback AP (because no
//! known WiFi was reachable on boot), `aeon-netwatch.sh` activates
//! iptables DNAT rules that redirect all wlan0 client TCP:80/443 to
//! the Pi. Port 443 hits the supervisor's main HTTPS listener. Port 80
//! hits THIS listener, which:
//!
//!   - Recognises the well-known captive-probe URLs each major OS
//!     pings on every new network attach (Apple, Android, Microsoft,
//!     and a few others). Each is answered in the specific way that
//!     triggers that OS's "this network requires sign-in" UI.
//!   - Redirects everything else to `https://<gateway>/setup/wifi`.
//!
//! When AP-setup mode is NOT active, port 80 CAN still be hit — a user
//! who types `http://<pi-ip>` in a browser lands here directly (no DNAT
//! needed). In that case we must NOT shove them at the setup page (the
//! old bug: it redirected every port-80 request to the AP-gateway setup
//! URL, so a connected WiFi client navigating to the device's LAN IP got
//! bounced to an unreachable `https://192.168.50.1/setup/wifi`). Instead,
//! when not in AP mode, we simply upgrade http→https for the SAME host.
//!
//! AP mode is detected via the `/run/aeon/ap-mode` flag file, which
//! `aeon-netwatch.sh` touches when it brings the fallback AP up and
//! removes when it tears it down.

use axum::extract::Request;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use std::path::Path;
use tracing::{info, warn};

/// Where the captive-portal redirect points clients while in AP mode.
/// Has to match aeon-netwatch.sh's AP_GATEWAY.
const SETUP_REDIRECT: &str = "https://192.168.50.1/setup/wifi";

/// Flag file written by aeon-netwatch while the fallback AP is up.
/// Present ⇒ we're in AP-setup mode and should run the captive flow;
/// absent ⇒ normal client/operation, just upgrade http→https.
const AP_MODE_FLAG: &str = "/run/aeon/ap-mode";

/// Are we currently serving the `aeon-setup` fallback AP?
fn ap_mode_active() -> bool {
    Path::new(AP_MODE_FLAG).exists()
}

/// Not-in-AP-mode response: 301 the client to the HTTPS console on the
/// SAME host they asked for (strips any :port from the Host header).
/// This is what makes `http://aeon-magick.local` or `http://<lan-ip>`
/// land on the real web UI instead of the setup page.
fn upgrade_to_https(req: &Request) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "aeon-magick.local".to_string());
    let target = format!("https://{host}/");
    let mut resp = Redirect::permanent(&target).into_response();
    resp.headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    resp
}

/// Build the port-80 router. Routes:
///   - Apple: GET /hotspot-detect.html, /library/test/success.html
///   - Android: GET /generate_204, /gen_204
///   - Microsoft: GET /ncsi.txt, /connecttest.txt
///   - GET /  → redirect to setup
///   - GET *  → redirect to setup
///   - HEAD * → 200 (some captive detectors HEAD-probe first)
pub fn router() -> Router {
    Router::new()
        // ── Apple (iOS, macOS) ──
        // Real captive.apple.com returns:
        //   <HTML><HEAD><TITLE>Success</TITLE></HEAD><BODY>Success</BODY></HTML>
        // Anything OTHER than that body → captive sheet pops.
        .route("/hotspot-detect.html", get(apple_probe))
        .route("/library/test/success.html", get(apple_probe))
        // ── Android (and Chrome OS) ──
        // Expects HTTP 204 No Content. Anything else (we send 302) → popup.
        .route("/generate_204", get(android_probe))
        .route("/gen_204", get(android_probe))
        // ── Microsoft (Windows) ──
        // Expects /ncsi.txt to return "Microsoft NCSI" literally.
        // We respond with a redirect → Windows shows "no internet, sign in".
        .route("/ncsi.txt", get(windows_probe))
        .route("/connecttest.txt", get(windows_probe))
        // ── Kindle / Amazon Fire ──
        .route("/kindle-wifi/wifistub.html", get(generic_probe))
        // ── Mozilla / Firefox ──
        .route("/canonical.html", get(generic_probe))
        // Anything else → redirect to setup
        .fallback(catchall)
}

/// Apple captive-probe response: any body OTHER than the "Success"
/// HTML triggers the captive sheet. We serve a meta-refresh that also
/// works if the user clicks through manually.
async fn apple_probe(req: Request) -> Response {
    if !ap_mode_active() {
        return upgrade_to_https(&req);
    }
    let body = format!(
        r#"<!DOCTYPE html>
<html>
<head>
<title>Aeon Magick — Setup</title>
<meta http-equiv="refresh" content="0; url={SETUP_REDIRECT}">
</head>
<body>
<p>Redirecting to <a href="{SETUP_REDIRECT}">Aeon Magick setup</a>…</p>
</body>
</html>"#
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response()
}

/// Android probe: 302 redirect breaks the "expect 204" check and
/// triggers Android's captive UI. We point straight at /setup/wifi.
async fn android_probe(req: Request) -> Response {
    if !ap_mode_active() {
        return upgrade_to_https(&req);
    }
    Redirect::to(SETUP_REDIRECT).into_response()
}

/// Windows probe: redirect away from /ncsi.txt. Windows shows
/// "internet access requires you to take action" in the system tray.
async fn windows_probe(req: Request) -> Response {
    if !ap_mode_active() {
        return upgrade_to_https(&req);
    }
    Redirect::to(SETUP_REDIRECT).into_response()
}

/// Generic probe handler — used for less-common OS-probe URLs we want
/// to redirect rather than serve.
async fn generic_probe(req: Request) -> Response {
    if !ap_mode_active() {
        return upgrade_to_https(&req);
    }
    Redirect::to(SETUP_REDIRECT).into_response()
}

/// Anything else hitting port 80.
///   - In AP-setup mode → redirect to the setup page (the captive flow).
///   - Otherwise → upgrade http→https for the same host (so a connected
///     client typing `http://<pi>` reaches the real console, not setup).
async fn catchall(req: Request) -> Response {
    if !ap_mode_active() {
        return upgrade_to_https(&req);
    }
    let mut resp = Redirect::to(SETUP_REDIRECT).into_response();
    // Add Connection: close so clients don't try to keepalive the
    // 80→443 redirect — they should reconnect to 443 fresh.
    resp.headers_mut().insert(
        header::CONNECTION,
        HeaderValue::from_static("close"),
    );
    resp
}

/// Bind port 80 and serve the captive router. Idempotent + tolerant
/// of bind failures (port 80 needs root or CAP_NET_BIND_SERVICE — the
/// systemd unit grants that capability).
pub async fn serve() {
    let listener = match tokio::net::TcpListener::bind("0.0.0.0:80").await {
        Ok(l) => l,
        Err(e) => {
            warn!(?e, "captive: could not bind port 80 (no CAP_NET_BIND_SERVICE?); captive portal disabled");
            return;
        }
    };
    info!("captive: HTTP listener on :80 ready");

    if let Err(e) = axum::serve(listener, router().into_make_service()).await {
        warn!(?e, "captive listener exited");
    }
}
