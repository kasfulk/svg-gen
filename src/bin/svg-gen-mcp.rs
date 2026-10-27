// MCP server for svg-gen. Two transports:
//   svg-gen-mcp                     stdio, newline-delimited JSON-RPC (default)
//   svg-gen-mcp --http 0.0.0.0:8099 Streamable HTTP: POST /mcp, GET /health
// Tools: svg_from_query (Commons search -> draw), svg_from_image (given url/path -> draw).
use serde_json::{json, Value};
use std::io::{self, BufRead, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use svg_gen::*;

fn main() {
    let mut http: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--http" => http = args.next(),
            "-h" | "--help" => {
                println!("usage: svg-gen-mcp [--http ADDR]\n  --http 0.0.0.0:8099  serve POST /mcp (needs SG_TOKEN unless loopback)");
                return;
            }
            other => {
                eprintln!("flag tidak dikenal: {other}");
                std::process::exit(2)
            }
        }
    }
    match http {
        Some(addr) => serve_http(&addr),
        None => serve_stdio(),
    }
}

fn serve_stdio() {
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(req) => {
                if let Some(resp) = handle(&req, None) {
                    emit(resp);
                }
            }
            Err(_) => emit(json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}})),
        }
    }
}

fn emit(v: Value) {
    let mut out = io::stdout();
    let _ = writeln!(out, "{v}"); // one line, never pretty-printed
    let _ = out.flush();
}

/// One JSON-RPC message in, at most one response out. None for notifications.
fn handle(req: &Value, out_base: Option<&Path>) -> Option<Value> {
    let id = req.get("id").cloned().filter(|i| !i.is_null())?;
    let resp = match req["method"].as_str().unwrap_or("") {
        "initialize" => json!({
            "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2025-03-26"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "svg-gen", "version": env!("CARGO_PKG_VERSION")}
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => call(&req["params"], out_base),
        other => return Some(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("unknown method {other}")}})),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":resp}))
}

fn ok(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": false})
}

fn err(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

fn tools() -> Value {
    json!([
        {
            "name": "svg_from_query",
            "description": "Cari foto di Wikimedia Commons untuk sebuah kata kunci (mis. \"gunung bromo\"), lalu model vision menulis ulang foto itu menjadi ilustrasi SVG flat. Mengembalikan SVG, path file, URL sumber, dan lisensi foto.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Kata kunci subjek, Bahasa Indonesia atau Inggris."},
                    "out": {"type": "string", "description": "Nama file SVG tujuan. Default: <query-slug>.svg. Saat server HTTP, path relatif terhadap SG_OUT_DIR (default ~/svg-gen-out) dan path absolut ditolak."},
                    "model": {"type": "string", "description": "Model vision. Default env SG_MODEL atau kn-b/deepseek-v4-1-flash."},
                    "mode": {"type": "string", "enum": ["scene", "object"], "description": "scene untuk pemandangan/lanskap, object untuk foto satu benda (studio, museum, produk). Default scene."}
                },
                "required": ["query"]
            }
        },
        {
            "name": "svg_from_image",
            "description": "Sama seperti svg_from_query tapi sumber gambarnya ditentukan sendiri: URL http(s) atau path file lokal. Berguna kalau fotonya sudah ada.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "URL gambar http(s)."},
                    "path": {"type": "string", "description": "Path gambar lokal (dipakai kalau url kosong)."},
                    "out": {"type": "string", "description": "Nama file SVG tujuan. Default: out.svg. Saat server HTTP, path dibatasi ke SG_OUT_DIR."},
                    "model": {"type": "string", "description": "Model vision. Default env SG_MODEL."},
                    "mode": {"type": "string", "enum": ["scene", "object"], "description": "scene atau object. Default scene."},
                    "subject": {"type": "string", "description": "Nama benda di gambar, dipakai mode object (mis. \"kapak\"). Default: diambil dari url/path."}
                }
            }
        }
    ])
}

fn call(params: &Value, out_base: Option<&Path>) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let a = &params["arguments"];
    let model = a["model"].as_str().map(str::to_string).unwrap_or_else(default_model);
    let mode = a["mode"].as_str().unwrap_or("scene").to_string();
    let subject = a["subject"]
        .as_str()
        .or_else(|| a["query"].as_str())
        .unwrap_or("the main subject")
        .to_string();

    // Validate the destination before spending a model call: a rejected path must
    // fail in milliseconds, not after 2 minutes of generation.
    let requested_out = a["out"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    if let (Some(base), Some(p)) = (out_base, requested_out.as_deref()) {
        if let Err(e) = sandbox(base, p) {
            return err(e);
        }
    }

    let (img, mime, src, license, default_out) = match name {
        "svg_from_query" => {
            let Some(query) = a["query"].as_str() else {
                return err("butuh field 'query'".into());
            };
            let photo = match find_image(query) {
                Ok(p) => p,
                Err(e) => return err(format!("Gagal cari foto: {e}")),
            };
            let img = match get(&photo.url) {
                Ok(i) => i,
                Err(e) => return err(format!("Gagal unduh {}: {e}", photo.url)),
            };
            let mime = mime_for(&photo.url);
            (img, mime, format!("{} ({})", photo.title, photo.url), photo.license, format!("{}.svg", slug(query)))
        }
        "svg_from_image" => {
            let (img, mime, src) = match (a["url"].as_str(), a["path"].as_str()) {
                (Some(u), _) if !u.is_empty() => match get(u) {
                    Ok(i) => (i, mime_for(u), u.to_string()),
                    Err(e) => return err(format!("Gagal unduh {u}: {e}")),
                },
                // remote caller must not read arbitrary host files
                (_, Some(p)) if !p.is_empty() => {
                    if out_base.is_some() {
                        return err("server HTTP: argumen 'path' tidak diaktifkan, pakai 'url'".into());
                    }
                    match std::fs::read(p) {
                        Ok(i) => {
                            let mime = if p.to_lowercase().ends_with(".png") { "image/png" } else { "image/jpeg" };
                            (i, mime, p.to_string())
                        }
                        Err(e) => return err(format!("Gagal baca {p}: {e}")),
                    }
                }
                _ => return err("butuh field 'url' atau 'path'".into()),
            };
            (img, mime, src, "unknown".to_string(), "out.svg".to_string())
        }
        other => return err(format!("tool tidak dikenal: {other}")),
    };

    let svg = match ask_svg(&img, mime, &model, &mode, &subject) {
        Ok(s) => s,
        Err(e) => return err(format!("Model gagal menulis SVG: {e}")),
    };

    let requested = requested_out.as_deref().unwrap_or(&default_out);
    let path = match out_base {
        Some(base) => match sandbox(base, requested) {
            Ok(p) => match write_svg(&svg, &p.to_string_lossy()) {
                Ok(p) => p,
                Err(e) => return err(format!("Gagal tulis {}: {e}", p.display())),
            },
            Err(e) => return err(e),
        },
        None => match write_svg(&svg, requested) {
            Ok(p) => p,
            Err(e) => return err(format!("Gagal tulis {requested}: {e}")),
        },
    };

    let payload = json!({
        "svg_path": path.display().to_string(),
        "source": src,
        "license": license,
        "model": model,
        "mode": mode,
        "shapes": shapes(&svg),
        "bytes": svg.len(),
        "svg": svg,
    });
    ok(serde_json::to_string_pretty(&payload).unwrap_or_default())
}

/// Remote callers may only write inside the output dir: no absolute paths, no `..`.
fn sandbox(base: &Path, requested: &str) -> Result<PathBuf, String> {
    let p = Path::new(requested);
    if p.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("path '{requested}' ditolak: pakai nama file relatif tanpa ../ atau /"));
    }
    Ok(base.join(p))
}

fn token() -> Option<String> {
    if let Ok(t) = std::env::var("SG_TOKEN") {
        if !t.is_empty() {
            return Some(t);
        }
    }
    let p = PathBuf::from(std::env::var("HOME").ok()?).join(".config/svg-gen/token");
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn out_base() -> PathBuf {
    std::env::var("SG_OUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join("svg-gen-out"))
}

fn serve_http(addr: &str) {
    let tok = token();
    let loopback = addr.starts_with("127.") || addr.starts_with("localhost") || addr.starts_with("[::1]");
    if !loopback && tok.is_none() {
        eprintln!("menolak bind {addr} tanpa token.");
        eprintln!("server ini memanggil LLM (biaya kredit) dan menulis file di host ini;");
        eprintln!("siapa pun yang bisa menjangkau port terbuka bisa memakainya.");
        eprintln!("set SG_TOKEN=<rahasia> atau tulis token di ~/.config/svg-gen/token, lalu ulangi.");
        std::process::exit(1);
    }
    let base = Arc::new(out_base());
    let _ = std::fs::create_dir_all(base.as_ref());
    let server = match tiny_http::Server::http(addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("gagal bind {addr}: {e}");
            std::process::exit(1)
        }
    };
    eprintln!(
        "svg-gen-mcp http://{addr}/mcp  auth:{}  out:{}{}",
        if tok.is_some() { "bearer" } else { "none (loopback)" },
        base.display(),
        if base.as_ref().exists() { "" } else { " (gagal dibuat)" }
    );
    for request in server.incoming_requests() {
        let tok = tok.clone();
        let base = Arc::clone(&base);
        std::thread::spawn(move || handle_http(request, tok.as_deref(), &base));
    }
}

fn handle_http(mut req: tiny_http::Request, tok: Option<&str>, base: &Path) {
    let method = req.method().as_str().to_string();
    let path = req.url().split('?').next().unwrap_or("").to_string();

    if method == "GET" && path == "/health" {
        let _ = req.respond(json_resp(200, json!({"status": "ok", "transport": "http", "tools": 2})));
        return;
    }
    if path != "/mcp" && path != "/" {
        let _ = req.respond(tiny_http::Response::from_string("not found").with_status_code(404));
        return;
    }
    if method != "POST" {
        // Streamable HTTP: no server-initiated stream here, so GET/DELETE are refused.
        let _ = req.respond(tiny_http::Response::from_string("use POST /mcp").with_status_code(405));
        return;
    }
    if let Some(expected) = tok {
        let given = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Authorization"))
            .map(|h| h.value.as_str().trim_start_matches("Bearer ").trim().to_string())
            .or_else(|| {
                req.headers()
                    .iter()
                    .find(|h| h.field.equiv("X-SG-Token"))
                    .map(|h| h.value.as_str().trim().to_string())
            });
        let good = match given {
            Some(g) => ct_eq(g.as_bytes(), expected.as_bytes()),
            None => false,
        };
        if !good {
            let _ = req.respond(json_resp(401, json!({"error": "unauthorized"})));
            return;
        }
    }

    let mut body = String::new();
    let read = req.as_reader().take(16 << 20).read_to_string(&mut body);
    if let Err(e) = read {
        let _ = req.respond(json_resp(400, json!({"error": format!("body: {e}")})));
        return;
    }
    let msg: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            let _ = req.respond(json_resp(
                200,
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":format!("parse error: {e}")}}),
            ));
            return;
        }
    };
    if msg.is_array() {
        let _ = req.respond(json_resp(
            200,
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"batch tidak didukung"}}),
        ));
        return;
    }
    match handle(&msg, Some(base)) {
        Some(resp) => {
            let _ = req.respond(json_resp(200, resp));
        }
        None => {
            let _ = req.respond(tiny_http::Response::empty(202)); // notification
        }
    }
}

fn json_resp(status: u16, v: Value) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let ct = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    tiny_http::Response::from_string(v.to_string()).with_status_code(status).with_header(ct)
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b) {
        d |= x ^ y;
    }
    d == 0
}