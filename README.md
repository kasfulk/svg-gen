# svg-gen

Query → foto Wikimedia Commons → deepseek vision → file SVG.

Rust, satu file, tiga dependency (`ureq`, `serde_json`, `base64`). Tanpa API key pencarian: Commons
tidak butuh auth dan fotonya CC-licensed.

```bash
cargo build --release
export OPENAI_API_KEY=...          # token proxy OpenAI-compatible
./target/release/svg-gen "gunung bromo" -o examples/bromo.svg
```

```
photo: File:Mount Bromo at sunrise, showing its volcanoes and Mount Semeru (background).jpg (CC BY-SA 3.0)
bytes: 68306
svg:   /.../examples/bromo.svg (3432 bytes, 21 shapes)
```

Hasil (`examples/bromo.svg`, dirender headless Chromium):

![render](examples/bromo.png)

## Alur

1. `find_image` — Commons `generator=search`, ambil bitmap pertama, thumbnail 768px.
2. `get` — unduh byte, base64, kirim sebagai `image_url` data-URL.
3. `ask_svg` — model menulis SVG lapis-per-lapis (prompt minta 8–25 shape, palet ≤10 warna,
   `viewBox="0 0 1024 768"`), jawaban dipotong ke `<svg>…</svg>`.
4. `run` — tulis file, laporkan jumlah shape.

Model default `kn-b/deepseek-v4-1-flash` (visi aktif, ctx 1M). Ganti lewat `--model` atau `SG_MODEL`.
Endpoint proxy lewat `SG_API` (default `http://127.0.0.1:20128/v1`).

## Batas jujur

Ini **bukan** vectorisasi. Model melihat foto lalu menggambar ulang secara interpretatif — hasilnya
ilustrasi flat yang masih dikenali (uji manual: legibilitas 8/10 untuk lanskap gunung), bukan salinan
presisi. Detail kecil, tekstur, dan warna gradasi halus hilang. Untuk fidelity tinggi pakai tracer
(`vtracer`, `potrace`); dua pendekatan ini saling melengkapi, bukan saling menggantikan.

Input gambar dikirim ke proxy LLM — jangan pakai untuk foto privat.

## Lisensi

Kode: MIT. Foto contoh: *Mount Bromo at sunrise, showing its volcanoes and Mount Semeru (background)*
oleh Thomas Hirsch (user Ravn), [CC BY-SA 3.0](http://creativecommons.org/licenses/by-sa/3.0/),
via Wikimedia Commons. Lisensi foto tidak mengikat kode.