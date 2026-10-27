// CLI: svg-gen "<query>" [-o out.svg] [--model M] [--mode scene|object] [-n]
use svg_gen::*;

fn main() {
    let mut args = std::env::args().skip(1);
    let query = match args.next() {
        Some(q) if !q.starts_with('-') => q,
        _ => {
            eprintln!("usage: svg-gen \"<query>\" [-o out.svg] [--model M] [--mode scene|object] [-n]");
            std::process::exit(2)
        }
    };
    let mut out = None;
    let mut model = default_model();
    let mut dry = false;
    let mut mode = "scene".to_string();
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" | "--out" => out = args.next(),
            "--model" => model = args.next().unwrap_or(model),
            "--mode" => mode = args.next().unwrap_or(mode),
            "-n" | "--dry-run" => dry = true,
            _ => {}
        }
    }
    if let Err(e) = run(&query, out, &model, &mode, dry) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run(query: &str, out: Option<String>, model: &str, mode: &str, dry: bool) -> Result<(), String> {
    let photo = find_image(query)?;
    println!("photo: {} ({})\n       {}", photo.title, photo.license, photo.url);
    if dry {
        return Ok(());
    }
    let img = get(&photo.url)?;
    println!("bytes: {}", img.len());
    let svg = ask_svg(&img, mime_for(&photo.url), model, mode, query)?;
    let path = out.unwrap_or_else(|| format!("{}.svg", slug(query)));
    let abs = write_svg(&svg, &path)?;
    println!("svg:   {} ({} bytes, {} shapes)", abs.display(), svg.len(), shapes(&svg));
    Ok(())
}