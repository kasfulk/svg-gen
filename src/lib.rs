// Shared core: query -> Wikimedia Commons photo -> deepseek vision -> SVG.
// Used by both the CLI (src/main.rs) and the MCP server (src/bin/svg-gen-mcp.rs).
use base64::Engine;
use serde_json::{json, Value};
use std::io::Read;
use std::path::PathBuf;

pub const UA: &str = "svg-gen/0.1 (local)";

pub const PROMPT: &str = r#"Redraw this photo as a flat-vector SVG illustration that is clearly recognisable as the same scene.
Rules:
- Output ONLY raw <svg>...</svg>. No markdown, no explanation, no comments.
- viewBox="0 0 1024 768", width/height omitted.
- Keep the composition: same horizon line, same silhouette of the main shapes, same light direction.
- 8 to 25 shapes. Layer back to front. Use <path> for organic silhouettes, <polygon> for ridges.
- Flat fills or simple 2-stop linearGradient. At most 10 colours, sampled from the photo.
- No <image>, no external href, no <text>, no filters."#;

/// For studio/product/museum shots: one subject, plain ground. The scene prompt
/// invents horizons and decorations here, which is why objects need their own rules.
pub const PROMPT_OBJECT: &str = r#"Redraw the single object in this photo as a clean flat-vector SVG.
Rules:
- Output ONLY raw <svg>...</svg>. No markdown, no explanation, no comments.
- viewBox="0 0 1024 768", width/height omitted.
- One subject, centred, large: same silhouette, same proportions, same orientation as the photo.
- Same parts and same colour per part. Do not add objects, scenery, borders or decoration.
- Background: one flat fill sampled from the photo.
- 5 to 20 shapes, flat fills only. No <image>, no external href, no <text>, no filters."#;

/// The subject name comes from the caller's query: without it the model guesses
/// what the object is (an "axe" photo once came back as a mallet).
pub fn prompt_for(mode: &str, subject: &str) -> String {
    match mode {
        "object" => format!("Subject: {subject}.\n{PROMPT_OBJECT}"),
        _ => PROMPT.to_string(),
    }
}

pub fn api() -> String {
    std::env::var("SG_API").unwrap_or_else(|_| "http://127.0.0.1:20128/v1".into())
}

pub fn default_model() -> String {
    std::env::var("SG_MODEL").unwrap_or_else(|_| "kn-b/deepseek-v4-1-flash".into())
}

pub struct Photo {
    pub url: String,
    pub title: String,
    pub license: String,
}

/// percent-encode everything except unreserved chars
pub fn enc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

pub fn get(url: &str) -> Result<Vec<u8>, String> {
    // Wikimedia returns 429 under burst load and the local proxy returns 503 when a
    // backend hiccups; both clear on a short backoff, so retry instead of dying.
    let mut last = String::new();
    for attempt in 0..3 {
        match ureq::get(url).set("User-Agent", UA).call() {
            Ok(resp) => {
                let mut buf = Vec::new();
                return resp.into_reader().read_to_end(&mut buf).map_err(|e| e.to_string()).map(|_| buf);
            }
            Err(ureq::Error::Status(code, _)) if code == 429 || code >= 500 => {
                last = format!("{url}: status code {code}");
                std::thread::sleep(std::time::Duration::from_secs(2 << attempt));
            }
            Err(e) => return Err(format!("{url}: {e}")),
        }
    }
    Err(last)
}

/// Rank a Commons hit: query words found in the file title, weighted by title length.
/// Full-text search matches file names loosely ("pisau" also returns
/// "Lambang Kabupaten Pulang Pisau"), so a long title with one matching word
/// must lose against a short title that is mostly the query.
fn score(title: &str, tokens: &[String]) -> (usize, usize) {
    let words: Vec<String> = title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    let matched = tokens.iter().filter(|w| words.contains(w)).count();
    let density = matched * 1000 / words.len().max(1);
    (density, matched)
}

/// Best bitmap on Wikimedia Commons for `query`. No API key, CC-licensed.
/// Queries `intitle:` first (title-relevant) then plain full text, then ranks by title overlap.
pub fn find_image(query: &str) -> Result<Photo, String> {
    let tokens: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(str::to_string)
        .collect();

    let mut pages: Vec<Value> = Vec::new();
    for term in [format!("filetype:bitmap intitle:{}", query.trim()), format!("filetype:bitmap {}", query.trim())] {
        let url = format!(
            "https://commons.wikimedia.org/w/api.php?action=query&generator=search\
             &gsrsearch={}&gsrnamespace=6&gsrlimit=8&prop=imageinfo\
             &iiprop=url%7Csize%7Cextmetadata&iiurlwidth=768&format=json",
            enc(&term)
        );
        let v: Value = serde_json::from_slice(&get(&url)?).map_err(|e| e.to_string())?;
        if let Some(obj) = v["query"]["pages"].as_object() {
            // object order is not stable; keep per-query relevance via "index"
            let mut batch: Vec<Value> = obj.values().cloned().collect();
            batch.sort_by_key(|p| p["index"].as_i64().unwrap_or(99));
            pages.extend(batch);
        }
    }
    let mut seen = std::collections::HashSet::new();
    pages.retain(|p| seen.insert(p["title"].as_str().unwrap_or("").to_string()));
    if pages.is_empty() {
        return Err(format!("no image for {query:?}"));
    }
    // highest density first, then most matched words, then intitle-first insertion order
    pages.sort_by_key(|p| std::cmp::Reverse(score(p["title"].as_str().unwrap_or(""), &tokens)));
    pages.truncate(6);

    let p = &pages[0];
    let ii = &p["imageinfo"][0];
    let url = ii["thumburl"].as_str().or_else(|| ii["url"].as_str()).ok_or("no image url")?;
    Ok(Photo {
        url: url.split('?').next().unwrap_or(url).to_string(),
        title: p["title"].as_str().unwrap_or("?").to_string(),
        license: ii["extmetadata"]["LicenseShortName"]["value"].as_str().unwrap_or("?").to_string(),
    })
}

pub fn extract_svg(content: &str) -> Option<String> {
    let s = content.find("<svg")?;
    let e = content[s..].find("</svg>")? + s + 6;
    Some(content[s..e].to_string())
}

pub fn shapes(svg: &str) -> usize {
    svg.matches("<path").count() + svg.matches("<polygon").count()
}

pub fn mime_for(url: &str) -> &'static str {
    if url.to_lowercase().ends_with(".png") { "image/png" } else { "image/jpeg" }
}

pub fn ask_svg(img: &[u8], mime: &str, model: &str, mode: &str, subject: &str) -> Result<String, String> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(img);
    let body = json!({
        "model": model, "max_tokens": 8000, "temperature": 0.3,
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": prompt_for(mode, subject)},
            {"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{b64}")}}
        ]}]
    });
    let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    let resp = ureq::post(&format!("{}/chat/completions", api()))
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

pub fn slug(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect()
}

pub fn write_svg(svg: &str, path: &str) -> Result<PathBuf, String> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
    }
    std::fs::write(path, format!("{svg}\n")).map_err(|e| e.to_string())?;
    std::path::absolute(path).map_err(|e| e.to_string())
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
    #[test]
    fn counts_shapes() {
        assert_eq!(shapes("<svg><path d=\"\"/><polygon/><path/></svg>"), 3);
    }
}