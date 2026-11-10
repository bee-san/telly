//! Makes the character dictionary for one AniList show, to see what it holds.
//!
//!   cargo run --release --example names_check -- 6702 <folder>
//!
//! Fetches the cast and portraits into <folder> (kept, so a second run is
//! quick), builds the ZIP there and prints its size and a few entries.

use std::io::Read;
use telly_lib::names;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let id: i64 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(6702);
    let dir = std::path::PathBuf::from(args.get(2).cloned().unwrap_or_else(|| std::env::temp_dir().join("telly-names-check").display().to_string()));
    std::fs::create_dir_all(&dir)?;
    let cached = dir.join(format!("{id}.json"));
    let started = std::time::Instant::now();
    let mut cast: names::Cast = match std::fs::read(&cached).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
        Some(c) => c,
        None => names::fetch_cast(id, |page| eprintln!("page {page}"))?,
    };
    eprintln!("{} characters in {:.1?}", cast.characters.len(), started.elapsed());
    let started = std::time::Instant::now();
    names::portraits(&mut cast, &dir.join("img"), |done, of| if done % 50 == 0 || done == of { eprintln!("portraits {done}/{of}") })?;
    eprintln!("portraits in {:.1?}", started.elapsed());
    std::fs::write(&cached, serde_json::to_vec(&cast)?)?;
    let mut roles = std::collections::BTreeMap::new();
    for c in &cast.characters {
        *roles.entry(format!("{}{}", c.role, if c.name_original.is_empty() { " (no Japanese name)" } else { "" })).or_insert(0) += 1;
    }
    eprintln!("roles: {roles:?}");
    let started = std::time::Instant::now();
    let (bytes, summary) = names::build(&[cast.clone()], &dir.join("img"), false)?;
    std::fs::write(dir.join("telly-characters.zip"), &bytes)?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes))?;
    let mut entries = 0;
    let mut json_bytes = 0;
    let mut sample = Vec::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        if file.name().is_ok_and(|n| n.starts_with("term_bank_")) {
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            json_bytes += text.len();
            let list: Vec<serde_json::Value> = serde_json::from_str(&text)?;
            entries += list.len();
            sample.extend(list.iter().take(3).map(|e| format!("{} [{}]", e[0], e[1])));
        }
    }
    eprintln!(
        "built in {:.1?}: {} characters, {entries} entries, {:.1} MB of JSON, ZIP {:.1} MB",
        started.elapsed(),
        summary.characters,
        json_bytes as f64 / 1e6,
        bytes.len() as f64 / 1e6
    );
    eprintln!("sample: {}", sample.join(", "));
    let named = names::named_in(&cast, "ナツ！ルーシィ、グレイとエルザを呼んで。ハッピーも", 10);
    eprintln!("named: {named:?}");
    Ok(())
}
