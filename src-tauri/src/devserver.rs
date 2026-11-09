//! A development server that answers the UI's requests with the real backend,
//! over HTTP, so the frontend can run in an ordinary browser (src/lib/api.ts
//! falls back to it outside Tauri). Not part of the app.
//!
//!   npm run devserver -- <data dir> [port]
//!
//!   POST /call   {"cmd": "...", "args": {...}}  →  {"ok": value} or {"error": "..."}
//!   GET  /media/ep/<id>, /media/cover/<id>      →  what media:// serves in the app

use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use telly_lib::backend::{Backend, Dirs};

/// Scripts (no Origin) and pages served from this computer may call; a website
/// open in a browser meanwhile may not.
fn allowed(origin: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    let host = origin.strip_prefix("http://").and_then(|rest| rest.split([':', '/']).next()).unwrap_or("");
    host == "localhost" || host == "127.0.0.1"
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: telly-devserver <data dir> [port]"));
    let dir = std::path::absolute(&dir).unwrap_or(dir);
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(1430);
    let backend = Arc::new(Backend::open(Dirs { local: dir.clone(), roaming: dir.clone() }).expect("opening the data folder"));
    backend.start();
    if std::env::var_os("TELLY_MINING").is_some() {
        backend.start_mining();
    }
    let server = tiny_http::Server::http(("127.0.0.1", port)).expect("starting the server");
    println!("telly-devserver on http://127.0.0.1:{port}, data in {}", dir.display());
    for mut request in server.incoming_requests() {
        let backend = backend.clone();
        std::thread::spawn(move || {
            let origin = request.headers().iter().find(|h| h.field.equiv("Origin")).map(|h| h.value.as_str().to_string());
            if !allowed(origin.as_deref()) {
                let _ = request.respond(tiny_http::Response::from_string("only pages on this computer").with_status_code(403));
                return;
            }
            let cors = tiny_http::Header::from_bytes("Access-Control-Allow-Origin", origin.as_deref().unwrap_or("*")).unwrap();
            let url = request.url().to_string();
            if request.method() == &tiny_http::Method::Options {
                let _ = request.respond(
                    tiny_http::Response::empty(204)
                        .with_header(cors)
                        .with_header(tiny_http::Header::from_bytes("Access-Control-Allow-Headers", "content-type, range").unwrap()),
                );
                return;
            }
            if let Some(path) = url.strip_prefix("/media") {
                let range = request.headers().iter().find(|h| h.field.equiv("Range")).map(|h| h.value.as_str().to_string());
                let response = match backend.serve(path, range.as_deref()) {
                    Ok(served) => {
                        let mut r = tiny_http::Response::from_data(served.bytes)
                            .with_header(tiny_http::Header::from_bytes("Content-Type", served.mime).unwrap())
                            .with_header(tiny_http::Header::from_bytes("Accept-Ranges", "bytes").unwrap())
                            .with_header(tiny_http::Header::from_bytes("Access-Control-Expose-Headers", "Content-Range").unwrap());
                        if let Some((start, end, total)) = served.range {
                            r = r
                                .with_status_code(206)
                                .with_header(tiny_http::Header::from_bytes("Content-Range", format!("bytes {start}-{end}/{total}")).unwrap());
                        }
                        r
                    }
                    Err(e) => tiny_http::Response::from_data(format!("{e:#}").into_bytes()).with_status_code(404),
                };
                let _ = request.respond(response.with_header(cors));
                return;
            }
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            let reply = match serde_json::from_str::<Value>(&body) {
                Ok(v) => {
                    let cmd = v.get("cmd").and_then(Value::as_str).unwrap_or("").to_string();
                    let a = v.get("args").cloned().unwrap_or(json!({}));
                    match backend.call(&cmd, a) {
                        Ok(value) => json!({ "ok": value }),
                        Err(e) => json!({ "error": format!("{e:#}") }),
                    }
                }
                Err(e) => json!({ "error": e.to_string() }),
            };
            let _ = request.respond(
                tiny_http::Response::from_string(reply.to_string())
                    .with_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap())
                    .with_header(cors),
            );
        });
    }
}
