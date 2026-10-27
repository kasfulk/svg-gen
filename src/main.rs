// svg-gen: query -> Wikimedia Commons photo -> deepseek vision -> SVG file.
// Port of ../svg-gen/gen.py. Usage: svg-gen "gunung bromo" [-o out.svg] [--model M]
use base64::Engine;
use serde_json::{json, Value};
use std::io::Read;

const UA: &str = "svg-gen/0.1 (local)";
const API: &str = "http://127.0.0.1:20128/v1";

const PROMPT: &str = r#"Redraw this photo as a flat-vector SVG illustration that is clearly recognisable as the same scene.
Rules:
- Output ONLY raw <svg>...</svg>. No markdown, no explanation, no comments.
- viewBox="0 0 1024 768", width/height omitted.
- Keep the composition: same horizon line, same silhouette of the main shapes, same light direction.
- 8 to 25 shapes. Layer back to front. Use <path> for organic silhouettes, <polygon> for ridges.
- Flat fills or simple 2-stop linearGradient. At most 10 colours, sampled from the photo.
- No <image>, no external href, no <text>, no filters."#;

// percent-encode everything except unreserved chars
fn enc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

fn get(url: &str) -> Result<Vec<u8>, String> {
    let resp = ureq::get(url).set("User-Agent", UA).call().map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

/// photo url, title, license
fn find_image(query: &str) -> Result<(String, String, String), String> {
    let url = format!(
        "https://commons.wikimedia.org/w/api.php?action=query&generator=search\
         &gsrsearch={}&gsrnamespace=6&gsrlimit=5&prop=imageinfo\
         &iiprop=url%7Csize%7Cextmetadata&iiurlwidth=768&format=json",
        enc(&format!("filetype:bitmap {query}"))
    );
    let v: Value = serde_json::from_slice(&get(&url)?).map_err(|e| e.to_string())?;
    let mut pages: Vec<&Value> = v["query"]["pages"]
        .as_object()
        .ok_or_else(|| format!("no image for {query:?}"))?
        .values()
        .collect();
    pages.sort_by_key(|p| p["index"].as_i64().unwrap_or(99));
    let p = pages.first().ok_or("empty result set")?;
    let ii = &p["imageinfo"][0];
    let url = ii["thumburl"].as_str().or_else(|| ii["url"].as_str()).ok_or("no image url")?;
    let title = p["title"].as_str().unwrap_or("?");
    let lic = ii["extmetadata"]["LicenseShortName"]["value"].as_str().unwrap_or("?");
    Ok((url.split('?').next().unwrap_or(url).to_string(), title.to_string(), lic.to_string()))
}

fn extract_svg(content: &str) -> Option<String> {
    let s = content.find("<svg")?;
    let e = content[s..].find("</svg>")? + s + 6;
    Some(content[s..e].to_string())
}

fn ask_svg(img: &[u8], mime: &str, model: &str) -> Result<String, String> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(img);
    let body = json!({
        "model": model, "max_tokens": 8000, "temperature": 0.3,
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": PROMPT},
            {"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{b64}")}}
        ]}]
    });
    let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    let resp = ureq::post(&format!("{API}/chat/completions"))
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {key}"))
        .send_string(&body.to_string())
        .map_err(|e| e.to_string())?;
    let mut txt = String::new();
    resp.into_reader().read_to_string(&mut txt).map_err(|e| e.to_string())?;
    let txt = txt.split("data: [DONE]").next().unwrap_or(&txt);
    let v: Value = serde_json::from_str(txt).map_err(|e| format!("{e}: {}", &txt[..txt.len().min(300)]))?;
    let content = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
    extract_svg(content).ok_or_else(|| format!("no <svg> in reply: {}", &content[..content.len().min(600)]))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let query = match args.next() {
        Some(q) if !q.starts_with('-') => q,
        _ => {
            eprintln!("usage: svg-gen \"<query>\" [-o out.svg] [--model M]");
            std::process::exit(2)
        }
    };
    let mut out = None;
    let mut model = std::env::var("SG_MODEL").unwrap_or_else(|_| "kn-b/deepseek-v4-1-flash".into());
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" | "--out" => out = args.next(),
            "--model" => model = args.next().unwrap_or(model),
            _ => {}
        }
    }

    if let Err(e) = run(&query, out, &model) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run(query: &str, out: Option<String>, model: &str) -> Result<(), String> {
    let (url, title, lic) = find_image(query)?;
    println!("photo: {title} ({lic})\n       {url}");
    let img = get(&url)?;
    println!("bytes: {}", img.len());
    let mime = if url.to_lowercase().ends_with(".png") { "image/png" } else { "image/jpeg" };
    let svg = ask_svg(&img, mime, model)?;

    let path = out.unwrap_or_else(|| {
        format!("{}.svg", query.chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect::<String>())
    });
    if let Some(dir) = std::path::Path::new(&path).parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
    }
    std::fs::write(&path, format!("{svg}\n")).map_err(|e| e.to_string())?;
    let shapes = svg.matches("<path").count() + svg.matches("<polygon").count();
    println!("svg:   {} ({} bytes, {shapes} shapes)", std::path::absolute(&path).map_err(|e| e.to_string())?.display(), svg.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enc_escapes_space_and_colon() {
        assert_eq!(enc("a b:c"), "a%20b%3Ac");
    }
    #[test]
    fn extracts_plain_and_noisy_svg() {
        assert_eq!(extract_svg("<svg><path/></svg>").unwrap(), "<svg><path/></svg>");
        assert_eq!(extract_svg("bla\n```xml\n<svg>x</svg>\n```").unwrap(), "<svg>x</svg>");
        assert!(extract_svg("nothing").is_none());
    }
}