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
//! When AP-setup mode is NOT active, port 80 isn't normally hit (no
//! DNAT in place), so the listener idles. We bind unconditionally and
//! let iptables decide whether traffic reaches us.

use axum::extract::Request;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use tracing::{info, warn};

/// Where the captive-portal redirect points clients.
/// Has to match aeon-netwatch.sh's AP_GATEWAY.
const SETUP_REDIRECT: &str = "https://192.168.50.1/setup/wifi";

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
async fn apple_probe() -> Response {
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
async fn android_probe() -> Response {
    Redirect::to(SETUP_REDIRECT).into_response()
}

/// Windows probe: redirect away from /ncsi.txt. Windows shows
/// "internet access requires you to take action" in the system tray.
async fn windows_probe() -> Response {
    Redirect::to(SETUP_REDIRECT).into_response()
}

/// Generic probe handler — used for less-common OS-probe URLs we want
/// to redirect rather than serve.
async fn generic_probe() -> Response {
    Redirect::to(SETUP_REDIRECT).into_response()
}

/// Anything else hitting port 80 — redirect to the setup page.
/// Browsers seeing a 302 immediately follow it. The OS captive
/// popups (already triggered by the OS-specific probe URLs above)
/// land users here too.
async fn catchall(_req: Request) -> Response {
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
