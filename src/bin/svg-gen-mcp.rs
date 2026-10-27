// MCP stdio server: newline-delimited JSON-RPC 2.0. Protocol on stdout, logs on stderr.
// Tools: svg_from_query (Commons search -> draw), svg_from_image (given url/path -> draw).
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use svg_gen::*;

fn main() {
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                emit(json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}}));
                continue;
            }
        };
        // no id => notification (notifications/initialized): never answer
        let Some(id) = req.get("id").cloned().filter(|i| !i.is_null()) else { continue };
        let method = req["method"].as_str().unwrap_or("");
        let resp = match method {
            "initialize" => json!({
                "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2024-11-05"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "svg-gen", "version": env!("CARGO_PKG_VERSION")}
            }),
            "ping" => json!({}),
            "tools/list" => json!({"tools": tools()}),
            "tools/call" => call(&req["params"]),
            other => {
                emit(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("unknown method {other}")}}));
                continue;
            }
        };
        emit(json!({"jsonrpc":"2.0","id":id,"result":resp}));
    }
}

fn emit(v: Value) {
    let mut out = io::stdout();
    let _ = writeln!(out, "{v}"); // one line, never pretty-printed
    let _ = out.flush();
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
                    "out": {"type": "string", "description": "Path file SVG tujuan. Default: <query-slug>.svg di cwd."},
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
                    "out": {"type": "string", "description": "Path file SVG tujuan. Default: out.svg di cwd."},
                    "model": {"type": "string", "description": "Model vision. Default env SG_MODEL."},
                    "mode": {"type": "string", "enum": ["scene", "object"], "description": "scene atau object. Default scene."},
                    "subject": {"type": "string", "description": "Nama benda di gambar, dipakai mode object (mis. \"kapak\"). Default: diambil dari url/path."}
                }
            }
        }
    ])
}

fn call(params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let a = &params["arguments"];
    let model = a["model"].as_str().map(str::to_string).unwrap_or_else(default_model);
    let mode = a["mode"].as_str().unwrap_or("scene").to_string();
    let subject = a["subject"]
        .as_str()
        .or_else(|| a["query"].as_str())
        .unwrap_or("the main subject")
        .to_string();

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
                (_, Some(p)) if !p.is_empty() => match std::fs::read(p) {
                    Ok(i) => {
                        let mime = if p.to_lowercase().ends_with(".png") { "image/png" } else { "image/jpeg" };
                        (i, mime, p.to_string())
                    }
                    Err(e) => return err(format!("Gagal baca {p}: {e}")),
                },
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
    let out = a["out"].as_str().filter(|s| !s.is_empty()).unwrap_or(&default_out);
    let path = match write_svg(&svg, out) {
        Ok(p) => p,
        Err(e) => return err(format!("Gagal tulis {out}: {e}")),
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