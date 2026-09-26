//! Signatuuripohjainen palautus (carving) raakasektoreista.
//!
//! Etsii tunnetut tiedostoalkut ja -loput koko lähteestä ja kirjoittaa
//! osumat tiedostoiksi virran kautta (streamaus; ei lataa kokonaisia
//! tiedostoja muistiin). Väärät osumat suodatetaan rakennetarkistuksilla.

use crate::rawdev::{find_all, human, out_path, sanitize, Device};
use std::io::{self, Write};
use std::path::PathBuf;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;

/// Yhden palautetun tiedoston yhteenveto.
#[derive(Clone, Debug)]
#[allow(dead_code)] // kentät ovat hyödyllistä metatietoa kutsujalle
pub struct CarveResult {
    pub path: PathBuf,
    pub kind: String,
    pub start_lba: u64,
    pub start_offset: usize,
    pub size: u64,
}

/// Yksi signatuurisääntö: alku + sallittu kokoväli.
struct Rule {
    kind: &'static str,
    start: &'static [u8],
    max_len: u64,
    min_len: u64,
}

fn rules() -> Vec<Rule> {
    vec![
        Rule { kind: "jpg", start: b"\xFF\xD8\xFF", max_len: 64 * MIB, min_len: 48 },
        Rule { kind: "png", start: b"\x89PNG\r\n\x1A\n", max_len: 256 * MIB, min_len: 40 },
        Rule { kind: "gif", start: b"GIF8", max_len: 32 * MIB, min_len: 13 },
        Rule { kind: "pdf", start: b"%PDF-", max_len: 256 * MIB, min_len: 24 },
        Rule { kind: "zip", start: b"PK\x03\x04", max_len: 512 * MIB, min_len: 40 },
        Rule { kind: "riff", start: b"RIFF", max_len: 2 * GIB, min_len: 36 },
        Rule { kind: "mp4", start: b"ftyp", max_len: 8 * GIB, min_len: 24 },
        Rule { kind: "bmp", start: b"BM", max_len: 256 * MIB, min_len: 66 },
        Rule { kind: "sqlite", start: b"SQLite format 3\x00", max_len: 2 * GIB, min_len: 100 },
    ]
}
/// Käy koko lähteen läpi 1 MiB:n lohkoina ja etsii kuvioiden esiintymät.
/// Palauttaa (absoluuttinen tavuoffset, kuvion indeksi). Lohkon häntä
/// säilytetään, jotta lohkirajan yli menevät kuviot löytyvät.
fn scan_patterns(dev: &mut Device, pats: &[&[u8]]) -> io::Result<Vec<(u64, usize)>> {
    let chunk: u64 = 1024 * 1024;
    let max_needle = pats.iter().map(|p| p.len()).max().unwrap_or(1) as u64;
    let device_end = dev.total_sectors().unwrap_or(0) * 512;
    let mut hits = Vec::new();
    let mut pos: u64 = 0;
    let mut next_report = 512 * MIB;
    while pos < device_end {
        let want = (device_end - pos).min(chunk + max_needle - 1) as usize;
        let buf = dev.read_at(pos, want)?;
        if buf.is_empty() {
            break;
        }
        let buf_len = buf.len() as u64;
        for (pi, p) in pats.iter().enumerate() {
            if p.is_empty() || buf_len < p.len() as u64 {
                continue;
            }
            for off in find_all(&buf, p, usize::MAX) {
                // Ohita kuvio, joka ei mahdu kokonaan tähän lohkoon
                // (se löytyy seuraavasta lohkosta, jonka häntä kattaa sen).
                if off as u64 + p.len() as u64 > buf_len {
                    continue;
                }
                hits.push((pos + off as u64, pi));
            }
        }
        if buf_len < want as u64 {
            break;
        }
        // Edist�: lohkon pituus miinus kuvion maksimipituus miinus yksi.
        // Pienessä häntälohkossa ei saa tapahtua ylivuotoa eikä jäämistä
        // paikalleen, joten käytetään saturating-laskua ja vähintään 1.
        let advance = buf_len.saturating_sub(max_needle as u64 - 1).max(1);
        pos += advance;
        if pos >= next_report {
            println!("  ...tarkistettu {} ({} ehdokasta)", human(pos), hits.len());
            next_report += 512 * MIB;
        }
    }
    hits.sort_unstable();
    hits.dedup();
    Ok(hits)
}

/// Etsii `needle`-kuvion laitteelta väliltä [from, from + max_bytes).
/// Lukee lohkoina ja pitää edellisen lohkon hännän puskurissa, jotta
/// lohkirajan yli menevät osumat löytyvät. Palauttaa absoluuttisen
/// offsetin tai None.
fn stream_find(
    dev: &mut Device,
    from: u64,
    max_bytes: u64,
    needle: &[u8],
) -> io::Result<Option<u64>> {
    if needle.is_empty() {
        return Ok(None);
    }
    let chunk: u64 = 1024 * 1024;
    let keep = (needle.len() - 1) as u64;
    let mut pos = from;
    let mut tail: Vec<u8> = Vec::new();
    while pos < from + max_bytes {
        let want = (from + max_bytes - pos).min(chunk) as usize;
        let buf = dev.read_at(pos, want)?;
        if buf.is_empty() {
            break;
        }
        let hay_abs = pos - tail.len() as u64;
        let mut hay = std::mem::take(&mut tail);
        hay.extend_from_slice(&buf);
        if hay.len() >= needle.len() {
            for i in find_all(&hay, needle, usize::MAX) {
                let abs = hay_abs + i as u64;
                if abs >= from && abs < from + max_bytes {
                    return Ok(Some(abs));
                }
            }
        }
        let buf_len = buf.len() as u64;
        if buf_len < want as u64 {
            break;
        }
        pos += buf_len - keep;
        let take = (keep as usize).min(buf.len());
        tail = buf[buf.len() - take..].to_vec();
    }
    Ok(None)
}
/// Etsii lopun skannaamalla tiedoston rungosta (jpg, png, gif, pdf, zip).
fn end_by_search(
    dev: &mut Device,
    rule: &Rule,
    start: u64,
) -> io::Result<Option<(u64, &'static str)>> {
    match rule.kind {
        "jpg" => {
            // Vaadi järkevä segmentti heti SOI:n jälkeen (FF D8 FF xx).
            let head = dev.read_at(start, 6)?;
            if head.len() < 6 {
                return Ok(None);
            }
            let marker = head[3];
            let plausible = marker == 0xDB || (0xE0..=0xEF).contains(&marker);
            if !plausible {
                return Ok(None);
            }
            let seg_len = u16::from_be_bytes([head[4], head[5]]);
            if marker != 0xDB && seg_len < 2 {
                return Ok(None);
            }
            let end = stream_find(dev, start + 4, rule.max_len - 4, b"\xFF\xD9")?;
            Ok(end.map(|p| (p + 2, "jpg")))
        }
        "png" => {
            let end = stream_find(dev, start + 8, rule.max_len - 8, b"IEND")?;
            Ok(end.map(|p| (p + 8, "png")))
        }
        "gif" => {
            let head = dev.read_at(start, 13)?;
            if head.len() < 13 || &head[0..4] != b"GIF8" {
                return Ok(None);
            }
            let flags = head[10];
            let gct = if flags & 0x80 != 0 {
                3u64 * (1u64 << ((flags & 0x07) as u64 + 1))
            } else {
                0
            };
            let from = start + 13 + gct;
            let end = stream_find(dev, from, rule.max_len, b"\x3B")?;
            Ok(end.map(|p| (p + 1, "gif")))
        }
        "pdf" => {
            let end = stream_find(dev, start + 16, rule.max_len - 16, b"%%EOF")?;
            Ok(end.map(|p| {
                let mut e = p + 5;
                // %%EOF:n perässä voi olla rivinvaihtoja tai uusi merkintä.
                for _ in 0..4 {
                    match dev.read_at(e, 1) {
                        Ok(b)
                            if b.len() == 1
                                && (b[0] == b'\r' || b[0] == b'\n' || b[0] == b'%') =>
                        {
                            e += 1
                        }
                        _ => break,
                    }
                }
                (e, "pdf")
            }))
        }
        "zip" => {
            let found = stream_find(dev, start + 30, rule.max_len - 30, b"PK\x05\x06")?;
            let found = match found {
                Some(p) => p,
                None => return Ok(None),
            };
            let tail = dev.read_at(found + 8, 14)?;
            if tail.len() < 14 {
                return Ok(None);
            }
            // EOCD: [0..4] merkinnät, [4..8] cd-koko, [8..12] cd-alku, [12..14] kommentti.
            let cd_size = u32::from_le_bytes([tail[4], tail[5], tail[6], tail[7]]) as u64;
            let cd_offset = u32::from_le_bytes([tail[8], tail[9], tail[10], tail[11]]) as u64;
            let comment = u16::from_le_bytes([tail[12], tail[13]]) as u64;
            // Central directoryn on mahduttava tiedoston sisään.
            if cd_offset + cd_size > found - start {
                return Ok(None);
            }
            Ok(Some((found + 22 + comment, "zip")))
        }
        _ => Ok(None),
    }
}
/// Jäsentää lopun suoraan tiedoston otsikosta (riff, mp4, bmp, sqlite).
fn end_by_header(
    dev: &mut Device,
    rule: &Rule,
    start: u64,
) -> io::Result<Option<(u64, &'static str)>> {
    let head = dev.read_at(start, 64)?;
    match rule.kind {
        "riff" => {
            if head.len() < 12 {
                return Ok(None);
            }
            // KORJAUS: RIFF-koon kenttä on offsetissa 4 (ei 8) ja kertoo
            // tiedoston lopun suoraan: loppu = alku + 8 + koko.
            let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as u64;
            let tag = [head[8], head[9], head[10], head[11]];
            let tag_ok = tag.iter().all(|b| b.is_ascii_alphanumeric() || *b == b' ');
            if !tag_ok || size < 4 {
                return Ok(None);
            }
            let kind = match &tag {
                b"WAVE" => "wav",
                b"AVI " => "avi",
                b"WEBP" => "webp",
                _ => "riff",
            };
            Ok(Some((start + 8 + size, kind)))
        }
        "bmp" => {
            if head.len() < 30 {
                return Ok(None);
            }
            let size = u32::from_le_bytes([head[2], head[3], head[4], head[5]]) as u64;
            let header_size = u32::from_le_bytes([head[14], head[15], head[16], head[17]]);
            let bitcount = u16::from_le_bytes([head[28], head[29]]);
            let reserved_ok = head[6] == 0 && head[7] == 0 && head[8] == 0 && head[9] == 0;
            let bc_ok = matches!(bitcount, 1 | 4 | 8 | 16 | 24 | 32);
            if reserved_ok && bc_ok && (40..=124).contains(&header_size) && size >= 66 {
                Ok(Some((start + size, "bmp")))
            } else {
                Ok(None)
            }
        }
        "sqlite" => {
            if head.len() < 32 {
                return Ok(None);
            }
            let ps16 = u16::from_be_bytes([head[16], head[17]]);
            let page_size = if ps16 == 1 { 65536u64 } else { ps16 as u64 };
            let page_count =
                u32::from_be_bytes([head[28], head[29], head[30], head[31]]) as u64;
            let pow2 = page_size.is_power_of_two() && page_size >= 512;
            if pow2 && page_count >= 1 && page_size * page_count <= 2 * GIB {
                Ok(Some((start + page_size * page_count, "sqlite")))
            } else {
                Ok(None)
            }
        }
        "mp4" => {
            // KORJAUS: kävellään koko ylätason boxilista alusta (size BE u32 +
            // tyyppi), jolloin moov/mdat-loppu tulee oikein riippumatta siitä,
            // kumpi on ensin, eikä +8-lisäyksiä tarvita.
            let mut pos = start;
            let mut last_end = 0u64;
            let mut boxes = 0u32;
            while boxes < 64 && pos <= start + rule.max_len {
                let h = dev.read_at(pos, 8)?;
                if h.len() < 8 {
                    break;
                }
                let size = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as u64;
                let typ_ok = h[4..8].iter().all(|b| b.is_ascii_alphanumeric());
                if size == 1 {
                    let ext = dev.read_at(pos + 8, 8)?;
                    if ext.len() < 8 {
                        break;
                    }
                    let large = u64::from_be_bytes(ext[0..8].try_into().unwrap());
                    if large < 16 {
                        break;
                    }
                    pos += large;
                } else if size == 0 && typ_ok {
                    // Viittaa tiedoston loppuun: käytetään lähteen loppua.
                    last_end = start + rule.max_len;
                    break;
                } else if size < 8 || !typ_ok {
                    // Rikkinäinen boxi tai roskadata: lopeta kävely.
                    break;
                } else {
                    pos += size;
                }
                last_end = pos;
                boxes += 1;
            }
            if boxes >= 1 && last_end > start {
                Ok(Some((last_end, "mp4")))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}
/// Laskee signatuuriosuman todellisen loppupisteen ja tyypin.
/// Palauttaa None, jos rakenne ei ole uskottava (väärä osuma).
fn span_end(
    dev: &mut Device,
    rule: &Rule,
    start: u64,
) -> io::Result<Option<(u64, &'static str)>> {
    match rule.kind {
        "jpg" | "png" | "gif" | "pdf" | "zip" => end_by_search(dev, rule, start),
        "riff" | "mp4" | "bmp" | "sqlite" => end_by_header(dev, rule, start),
        _ => Ok(None),
    }
}

/// Kopioi välän [start, end) laitteesta tiedostoon virtaa pitäen.
fn write_span(dev: &mut Device, path: &PathBuf, start: u64, end: u64) -> io::Result<u64> {
    let mut file = std::fs::File::create(path)?;
    let chunk: u64 = 1024 * 1024;
    let mut pos = start;
    while pos < end {
        let want = (end - pos).min(chunk) as usize;
        let buf = dev.read_at(pos, want)?;
        if buf.is_empty() {
            break;
        }
        file.write_all(&buf)?;
        pos += buf.len() as u64;
    }
    Ok(pos - start)
}

/// Skannaa koko lähteen, palauttaa kaikki uskottavat osumat tiedostoiksi
/// `out_dir`-kansioon ja palauttaa yhteenvedon.
pub fn carve_all(dev: &mut Device, out_dir: &str) -> io::Result<Vec<CarveResult>> {
    let rule_list = rules();
    let pats: Vec<&[u8]> = rule_list.iter().map(|r| r.start).collect();
    let hits = scan_patterns(dev, &pats)?;
    println!("Signatuuriehdokkaita: {}.", hits.len());
    let device_end = dev.total_sectors().unwrap_or(0) * 512;
    let mut results: Vec<CarveResult> = Vec::new();
    let mut last_end: u64 = 0;
    for (abs, pi) in &hits {
        let rule = &rule_list[*pi];
        let start = if rule.kind == "mp4" {
            if *abs < 4 {
                continue;
            }
            *abs - 4
        } else {
            *abs
        };
        if start < last_end {
            continue; // osuu jo palautetun tiedoston sisään
        }
        let (end, kind) = match span_end(dev, rule, start)? {
            Some(v) => v,
            None => continue,
        };
        let len = end.saturating_sub(start);
        if len < rule.min_len || len > rule.max_len {
            continue;
        }
        if device_end > 0 && end > device_end {
            continue;
        }
        let name = sanitize(&format!(
            "carve_{:04}_{:09}.{}",
            results.len() + 1,
            start / 512,
            kind
        ));
        let path = out_path(out_dir, &name);
        let size = write_span(dev, &path, start, end)?;
        if size == 0 {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        println!(
            "-> {} ({} sektorilta {})",
            path.display(),
            human(size),
            start / 512
        );
        last_end = end;
        results.push(CarveResult {
            path,
            kind: kind.to_string(),
            start_lba: start / 512,
            start_offset: (start % 512) as usize,
            size,
        });
    }
    println!();
    if results.is_empty() {
        println!("Tunnettuja signatuureja ei löytynyt.");
    } else {
        println!("Signatuuripalautus valmis: {} tiedostoa.", results.len());
        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for r in &results {
            *counts.entry(r.kind.clone()).or_insert(0) += 1;
        }
        for (k, c) in counts {
            println!("  {}: {} kpl", k, c);
        }
    }
    Ok(results)
}

/// Vuorovaikutteinen signatuuripalautus (päävalikon toiminto).
pub fn run_carve(dev: &mut Device) -> io::Result<()> {
    println!();
    println!("=== SIGNATUURIPALAUTUS (carving) ===");
    println!("Tunnetut tiedostot etsitään koko lähteestä rakenteineen. Suurella");
    println!("kortilla skannaus kestää useita minuutteja.");
    println!();
    let results = carve_all(dev, "palautetut_carve")?;
    if results.is_empty() {
        println!("Palautettavia signatuureja ei löytynyt.");
    }
    Ok(())
}