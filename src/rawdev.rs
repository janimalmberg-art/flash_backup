//! Raakalähteen (asema tai levykuva) lukeminen ja tavutason apufunktiot.
//!
//! Kaikki laiteoperaatiot ovat vain luku -tyyppisiä: kortille EI kirjoiteta.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// SD-korttien tyypillinen looginen sektorikoko (tavua).
pub const SECTOR: u64 = 512;

/// Read + Seek -yhdistelmä dynaamista tallennusta varten:
/// joko Windowsin raakalaite tai tavallinen levykuvatiedosto.
#[allow(dead_code)] // varattu tuleviin tallennusratkaisuihin
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// Read + Write + Seek -yhdistelmä fatfs-kirjastolle (perinteinen palautus).
pub trait ReadWriteSeek: Read + Write + Seek {}
impl<T: Read + Write + Seek> ReadWriteSeek for T {}

/// Avattu raakalähde: volyymi (`\\.\D:`), fyysinen levy
/// (`\\.\PhysicalDrive1`) tai levykuvatiedosto (esim. .img / .dd).
pub struct Device {
    inner: Box<dyn ReadWriteSeek>,
    pub path: String,
    /// Koko tavuina, jos se saatiin selvitettyä.
    pub size: Option<u64>,
}

impl Device {
    fn from_file(mut file: File, path: String) -> Self {
        let size = match file.seek(SeekFrom::End(0)) {
            Ok(s) if s > 0 => {
                let _ = file.seek(SeekFrom::Start(0));
                Some(s)
            }
            _ => None,
        };
        Device {
            inner: Box::new(file),
            path,
            size,
        }
    }

    /// Avaa aseman raakalaitteena. Vaatii järjestelmänvalvojan oikeudet.
    pub fn open(drive_letter: &str) -> io::Result<Self> {
        let path = format!(r"\\.\{}:", drive_letter);
        let file = OpenOptions::new().read(true).open(&path)?;
        Ok(Device::from_file(file, path))
    }

    /// Avaa fyysisen levyn ilman asemakirjainta (esim. `PHYSICALDRIVE1`).
    pub fn open_physical(name: &str) -> io::Result<Self> {
        let path = format!(r"\\.\{}", name);
        let file = OpenOptions::new().read(true).open(&path)?;
        Ok(Device::from_file(file, path))
    }

    /// Avaa levykuvatiedoston (esim. kortista tehty .img- tai .dd-kopio).
    pub fn open_image(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).open(path)?;
        let display = path.display().to_string();
        Ok(Device::from_file(file, display))
    }

    /// Avaa lähteen automaattisesti: asematunnus (esim. `D` tai `D:`),
    /// fyysinen levy (esim. `PHYSICALDRIVE1`) tai levykuvatiedoston polku.
    pub fn open_any(spec: &str) -> io::Result<Self> {
        let s = spec.trim();
        let upper = s.to_uppercase();
        if upper.starts_with("PHYSICALDRIVE") {
            return Device::open_physical(&upper);
        }
        let no_colon = upper.trim_end_matches(':');
        // Yksi kirjain = asematunnus.
        if no_colon.len() == 1 {
            return Device::open(&no_colon);
        }
        let path = Path::new(s);
        if path.exists() {
            return Device::open_image(path);
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Lähdettä ei löytynyt: {}", s),
        ))
    }

    /// Palauttaa taustalla olevan Read+Write+Seek-lähteen (fatfs-käyttöön).
    pub fn detach(self) -> Box<dyn ReadWriteSeek> {
        self.inner
    }

    /// Lukee enintään `len` tavua kohdasta `offset`.
    ///
    /// Windows vaatii, että volyymin raakaluvut ovat sektorikohdistettuja,
    /// joten tämä funktio kohdistaa luvun automaattisesti ja palauttaa
    /// pyydetyn ikkunan sisällön. Palauttaa vähemmän tavuja, jos lähde loppuu.
    pub fn read_at(&mut self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let start = offset / SECTOR * SECTOR;
        let end = ((offset + len as u64 + SECTOR - 1) / SECTOR) * SECTOR;
        let total = (end - start) as usize;
        self.inner.seek(SeekFrom::Start(start))?;
        let mut raw = vec![0u8; total];
        let mut filled = 0usize;
        while filled < total {
            match self.inner.read(&mut raw[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }
        raw.truncate(filled);
        let skip = (offset - start) as usize;
        if skip >= raw.len() {
            return Ok(Vec::new());
        }
        raw.drain(..skip);
        let avail = raw.len().min(len);
        raw.truncate(avail);
        Ok(raw)
    }

    /// Lukee `count` sektoria sektorista `start` alkaen.
    pub fn read_sectors(&mut self, start: u64, count: u64) -> io::Result<Vec<u8>> {
        self.read_at(start * SECTOR, count as usize * SECTOR as usize)
    }

    /// Lähteen koko sektoreina (jos tiedossa).
    pub fn total_sectors(&self) -> Option<u64> {
        self.size.map(|s| s / SECTOR)
    }
}
/// Etsii käytettävissä olevat asematunnukset (A..Z) raportointia varten.
#[allow(dead_code)] // käytetään vuorovaikutteisessa valikossa myöhemmin
pub fn list_volumes() -> Vec<String> {
    let mut out = Vec::new();
    for n in b'A'..=b'Z' {
        let letter = n as char;
        if Path::new(&format!("{}:\\", letter)).metadata().is_ok() {
            out.push(letter.to_string());
        }
    }
    out
}

/// Tulostaa muistilohkon hexdumppina, 16 tavua per rivi.
pub fn hexdump(buf: &[u8], base_offset: u64) {
    for (row, chunk) in buf.chunks(16).enumerate() {
        print!("{:08X}  ", base_offset + (row * 16) as u64);
        for i in 0..16 {
            match chunk.get(i) {
                Some(b) => print!("{:02X} ", b),
                None => print!("   "),
            }
            if i == 7 {
                print!(" ");
            }
        }
        print!(" |");
        for b in chunk {
            if b.is_ascii_graphic() || *b == b' ' {
                print!("{}", *b as char);
            } else {
                print!(".");
            }
        }
        println!("|");
    }
}

/// Etsii `needle`-tavujonon esiintymät `haystack`ista (enintään `limit`).
pub fn find_all(haystack: &[u8], needle: &[u8], limit: usize) -> Vec<usize> {
    let mut out = Vec::new();
    if needle.is_empty() || haystack.len() < needle.len() {
        return out;
    }
    'ulkoinen: for i in 0..=(haystack.len() - needle.len()) {
        for (j, n) in needle.iter().enumerate() {
            if haystack[i + j] != *n {
                continue 'ulkoinen;
            }
        }
        out.push(i);
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// Lukutapa ihmisluettavaksi: B / KiB / MiB / GiB.
pub fn human(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{} {}", bytes, units[0])
    } else {
        format!("{:.2} {}", v, units[u])
    }
}

/// Varmistaa tuloshakemiston olemassaolon ja palauttaa uniikin polun tiedostolle.
pub fn out_path(dir: &str, name: &str) -> PathBuf {
    let dir = Path::new(dir);
    let _ = std::fs::create_dir_all(dir);
    let base = dir.join(name);
    if !base.exists() {
        return base;
    }
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tiedosto".to_string());
    let ext = base
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    for i in 2.. {
        let candidate = if ext.is_empty() {
            dir.join(format!("{}_{}", stem, i))
        } else {
            dir.join(format!("{}_{}.{}", stem, i, ext))
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

/// Korvaa tiedostonimistä Windowsissa kielletyt merkit alaviivalla.
pub fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 32 => '_',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_all_occurrences() {
        let hay = b"ab_cd_ab__ab";
        assert_eq!(find_all(hay, b"ab", 10), vec![0, 6, 10]);
    }

    #[test]
    fn finds_nothing_in_empty() {
        assert!(find_all(b"abc", b"", 10).is_empty());
        assert!(find_all(b"", b"abc", 10).is_empty());
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(512), "512 B");
        assert_eq!(human(2048), "2.00 KiB");
        assert_eq!(human(5 * 1024 * 1024), "5.00 MiB");
    }
}