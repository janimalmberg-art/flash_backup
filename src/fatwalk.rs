//! Oma FAT-hakemistolukija: käy läpi elävät JA poistetut merkinnät.
//!
//! Tukee LFN-pitkiä nimiä (tarkistussummalla suojattuna), alikansioita
//! rekursiivisesti sekä poistettujen merkintöjen (0xE5) löytämistä.

use crate::fatinfo::{cluster_chain, FatInfo, FatType};
use crate::rawdev::{out_path, sanitize, Device};
use std::io::{self, Write};
use std::path::PathBuf;

/// Yksi hakemistomerkintä (elävä tai poistettu).
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u32,
    pub first_cluster: u32,
    pub deleted: bool,
    pub path: String,
}

/// Yksi LFN-merkinnän osa (pitkän nimen katkelma).
#[derive(Clone)]
struct LfnPart {
    order: u8,
    checksum: u8,
    deleted: bool,
    text: String,
}

/// 8.3-nimi 11 tavusta + NT-reservibiteistä (pienkirjainliput).
/// 0x05 ensimmäisenä tavuna tarkoittaa 0xE5:tä (ASCII '?').
fn name83_nt(name: &[u8; 11], nt: u8) -> String {
    let mut bytes = *name;
    if bytes[0] == 0x05 {
        bytes[0] = 0xE5;
    }
    let mut base = String::from_utf8_lossy(&bytes[0..8])
        .trim_end()
        .replace('\u{0000}', "?");
    let mut ext = String::from_utf8_lossy(&bytes[8..11])
        .trim_end()
        .replace('\u{0000}', "?");
    // NT-reservibitit: 0x08 = pääosan pienet kirjaimet, 0x04 = päätteen.
    if nt & 0x08 != 0 {
        base = base.to_lowercase();
    }
    if nt & 0x04 != 0 {
        ext = ext.to_lowercase();
    }
    if ext.is_empty() {
        base
    } else {
        format!("{}.{}", base, ext)
    }
}

/// Päättelee poistetun 8.3-nimen alkuperäisen ensimmäisen tavun:
/// kokeilee kaikkia 256 arvoa, kunnes LFN-osien tarkistussumma täsmää.
fn brute_first_byte(rest: &[u8], target: u8) -> Option<u8> {
    for cand in 0u8..=255 {
        let mut name = [0u8; 11];
        name[1..11].copy_from_slice(rest);
        name[0] = cand;
        if lfn_checksum(&name) == target {
            return Some(cand);
        }
    }
    None
}

/// Dekoodaa LFN-kentän UTF-16LE-tekstinä.
fn decode_utf16(raw: &[u8]) -> String {
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|p| u16::from_le_bytes([p[0], p[1]]))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches(|c| c == '\u{0000}' || c == '\u{FFFF}')
        .to_string()
}

/// LFN-tarkistussumma FAT-specin mukaan (8.3-nimen 11 tavusta).
pub(crate) fn lfn_checksum(name: &[u8; 11]) -> u8 {
    let mut sum: u16 = 0;
    for &b in name {
        sum = (((sum & 1) << 7) + b as u16 + (sum >> 1)) & 0xFF;
    }
    sum as u8
}

/// Yhdistää 8.3-merkinnän edellä olevat LFN-osat pitkäksi nimeksi.
///
/// Otetaan mukaan vain ne osat, joiden tarkistussumma täsmää 8.3-nimeen
/// ja jotka ovat samassa luokassa (elävä/poistettu). Tämä estää poistetun
/// tiedoston LFN-katkelmia sotkemasta seuraavien tiedostojen nimiä.
///
/// Järjestys: jos kaikilla valituilla osilla on sama order (poistetut,
/// kaikki 0xE5), tallennusjärjestys on tekstin käänteinen ja osat
/// käännetään. Muuten osat lajitellaan order-numeron mukaan.
fn merge_lfn_for(parts: &[LfnPart], checksum: Option<u8>, deleted: bool) -> Option<String> {
    let mut sel: Vec<&LfnPart> = parts
        .iter()
        .filter(|p| {
            checksum.map_or(true, |c| p.checksum == c)
                && p.deleted == deleted
                && !p.text.is_empty()
        })
        .collect();
    if sel.is_empty() {
        return None;
    }
    let same_order = sel.windows(2).all(|w| w[0].order == w[1].order);
    if same_order {
        sel.reverse();
    } else {
        sel.sort_by_key(|p| p.order);
    }
    let mut s = String::new();
    for p in sel {
        s.push_str(&p.text);
    }
    Some(s)
}
/// Jäsentää hakemistodataa (32-tavuiset merkinnät). Elävät ja poistetut
/// rivit palautetaan yhdessä; poistetuissa `deleted = true`.
pub fn parse_dir_entries(data: &[u8], parent_path: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut lfn: Vec<LfnPart> = Vec::new();
    let count = data.len() / 32;
    for i in 0..count {
        let b = &data[i * 32..(i + 1) * 32];
        let attr = b[11];
        // 0x00 tarkoittaa, ettei tämän kohdan jälkeen ole enää merkintöjä.
        if b[0] == 0x00 {
            break;
        }
        // Pitkän nimen merkintä (attribuutit 0x0F)
        if attr & 0x3F == 0x0F {
            let order = b[0] & 0x1F;
            if order == 0 {
                continue;
            }
            let mut raw = Vec::new();
            raw.extend_from_slice(&b[1..11]);
            raw.extend_from_slice(&b[14..26]);
            raw.extend_from_slice(&b[28..32]);
            lfn.push(LfnPart {
                order,
                checksum: b[13],
                deleted: b[0] == 0xE5,
                text: decode_utf16(&raw),
            });
            continue;
        }
        // Volyymimerkintä
        if attr & 0x08 != 0 {
            lfn.clear();
            continue;
        }
        let deleted = b[0] == 0xE5;
        let mut name_bytes = [0u8; 11];
        name_bytes.copy_from_slice(&b[0..11]);
        let nt = b[12];
        let name = if deleted {
            // Poistetussa eka tavu on 0xE5, joten 8.3-tarkistussummaa ei voi
            // laskea suoraan. Päättele alkuperäinen eka kirjain LFN-osien
            // summasta ja yhdistä vain saman luokan (poistetut) osat, jotta
            // edellisen poistetun tiedoston katkelmat eivät saasta nimiä.
            let consistent = !lfn.is_empty()
                && lfn.iter().all(|p| p.deleted)
                && lfn.iter().all(|p| p.checksum == lfn[0].checksum);
            if consistent {
                if let Some(letter) = brute_first_byte(&name_bytes[1..11], lfn[0].checksum) {
                    name_bytes[0] = letter;
                }
                merge_lfn_for(&lfn, None, deleted).unwrap_or_else(|| name83_nt(&name_bytes, nt))
            } else {
                name83_nt(&name_bytes, nt)
            }
        } else {
            let expected = lfn_checksum(&name_bytes);
            merge_lfn_for(&lfn, Some(expected), deleted)
                .unwrap_or_else(|| name83_nt(&name_bytes, nt))
        };
        lfn.clear();
        // Elävät polkupistemerkinnät ja poistetut nimettömät rivit ohitetaan
        if !deleted && (name == "." || name == "..") {
            continue;
        }
        if deleted && name == "?" {
            continue;
        }
        let first_cluster = cluster_of(b);
        let size = size_of(b);
        let path = if parent_path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", parent_path, name)
        };
        out.push(Entry {
            name,
            is_dir: attr & 0x10 != 0,
            size,
            first_cluster,
            deleted,
            path,
        });
    }
    out
}
/// 32-bittinen klusterinumero (FAT32: kahden 16-bittisen kentän yhdistelmä).
fn cluster_of(b: &[u8]) -> u32 {
    let lo = u16::from_le_bytes([b[26], b[27]]) as u32;
    let hi = u16::from_le_bytes([b[20], b[21]]) as u32;
    (hi << 16) | lo
}

/// Tiedoston koko tavuina.
fn size_of(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[28], b[29], b[30], b[31]])
}

/// Lukee klusteriketjun muistiin, enintään `cap` tavua.
///
/// Ketju seurataan FAT-taulusta jos `fat` on Some, muuten luetaan
/// peräkkäisiä klustereita (poistettu tiedosto, jonka taulumerkinnät
/// on tyhjennetty, tai kokonaan lukemattoman taulun tapaus).
pub fn read_chain(
    dev: &mut Device,
    info: &FatInfo,
    fat: Option<&[u8]>,
    first_cluster: u32,
    cap: u64,
) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let push = |dev: &mut Device, c: u32, out: &mut Vec<u8>| -> io::Result<bool> {
        if out.len() as u64 >= cap {
            return Ok(false);
        }
        let bytes = dev.read_sectors(info.lba_of_cluster(c), info.sectors_per_cluster as u64)?;
        if bytes.is_empty() {
            return Ok(false);
        }
        let take = ((cap - out.len() as u64) as usize).min(bytes.len());
        out.extend_from_slice(&bytes[..take]);
        Ok(true)
    };
    match fat {
        Some(f) => {
            for c in cluster_chain(f, info.fat_type, first_cluster) {
                if !push(dev, c, &mut out)? {
                    break;
                }
            }
        }
        None => {
            let mut c = first_cluster;
            while c >= 2 {
                if !push(dev, c, &mut out)? {
                    break;
                }
                c += 1;
            }
        }
    }
    Ok(out)
}

/// Kirjoittaa yhden klusterin tulosteeseen (kattoo `cap`-rajan).
fn write_one(
    dev: &mut Device,
    info: &FatInfo,
    c: u32,
    cap: u64,
    written: &mut u64,
    out: &mut dyn Write,
) -> io::Result<bool> {
    if *written >= cap {
        return Ok(false);
    }
    let bytes = dev.read_sectors(info.lba_of_cluster(c), info.sectors_per_cluster as u64)?;
    if bytes.is_empty() {
        return Ok(false);
    }
    let take = ((cap - *written) as usize).min(bytes.len());
    out.write_all(&bytes[..take])?;
    *written += take as u64;
    Ok(true)
}

/// Kirjoittaa klusteriketjun suoraan virralle (streamaus; ei lataa
/// monien gigatavujen tiedostoja muistiin).
fn write_chain(
    dev: &mut Device,
    info: &FatInfo,
    fat: Option<&[u8]>,
    first_cluster: u32,
    cap: u64,
    out: &mut dyn Write,
) -> io::Result<u64> {
    let mut written: u64 = 0;
    match fat {
        Some(f) => {
            for c in cluster_chain(f, info.fat_type, first_cluster) {
                if !write_one(dev, info, c, cap, &mut written, out)? {
                    break;
                }
            }
        }
        None => {
            let mut c = first_cluster;
            while c >= 2 {
                if !write_one(dev, info, c, cap, &mut written, out)? {
                    break;
                }
                c += 1;
            }
        }
    }
    Ok(written)
}

/// Lukee FAT12/FAT16:n juurihakemiston vakiopaikaltaan.
pub fn read_root_dir_fat12_16(dev: &mut Device, info: &FatInfo) -> io::Result<Vec<u8>> {
    let root_start = info.fat_start() + info.num_fats as u64 * info.fat_size_sectors as u64;
    dev.read_at(
        root_start * info.bytes_per_sector as u64,
        info.root_entries as usize * 32,
    )
}
/// Käy koko hakemistopuun läpi (elävät + poistetut) ja palauttaa listan.
pub fn scan_tree(
    dev: &mut Device,
    info: &FatInfo,
    fat: Option<&[u8]>,
    max_depth: usize,
) -> io::Result<Vec<Entry>> {
    let mut all = Vec::new();
    scan_dir(dev, info, fat, info.root_cluster, "", &mut all, 0, max_depth)?;
    Ok(all)
}

/// Lukee yhden hakemiston ja rekursioi alikansioihin.
fn scan_dir(
    dev: &mut Device,
    info: &FatInfo,
    fat: Option<&[u8]>,
    dir_cluster: u32,
    path: &str,
    out: &mut Vec<Entry>,
    depth: usize,
    max_depth: usize,
) -> io::Result<()> {
    let data = if depth == 0 && info.fat_type != FatType::Fat32 {
        read_root_dir_fat12_16(dev, info)?
    } else {
        // FAT saatavilla: luetaan koko ketju (hakemisto voi olla iso).
        // Ilman FAT:ia luetaan vain yksi klusteri perakkaisin.
        let cap = if fat.is_some() {
            16u64 * 1024 * 1024
        } else {
            (info.cluster_size() as u64).max(512).min(1024 * 1024)
        };
        read_chain(dev, info, fat, dir_cluster, cap)?
    };
    let entries = parse_dir_entries(&data, path);
    let mut subdirs = Vec::new();
    for e in &entries {
        if e.is_dir && e.first_cluster >= 2 {
            subdirs.push((e.first_cluster, e.path.clone()));
        }
    }
    out.extend(entries);
    if depth >= max_depth {
        return Ok(());
    }
    for (cluster, child_path) in subdirs {
        scan_dir(dev, info, fat, cluster, &child_path, out, depth + 1, max_depth)?;
    }
    Ok(())
}

/// Palauttaa yhden merkinnän tiedostoon `out_dir`-kansioon.
///
/// Palauttaa (polku, ketju_puuttui): ketju_puuttui = true, kun data
/// luettiin peräkkäisinä klustereina (poistetun taulumerkinnät oli
/// tyhjennetty tai FAT-taulua ei ollut saatavilla).
pub fn recover_entry(
    dev: &mut Device,
    info: &FatInfo,
    fat: Option<&[u8]>,
    entry: &Entry,
    out_dir: &str,
) -> io::Result<(PathBuf, bool)> {
    let path = out_path(out_dir, &sanitize(&entry.name));
    let file = std::fs::File::create(&path)?;
    let mut out = io::BufWriter::new(file);
    // Elävä tiedosto seuraa FAT-ketjua; poistettu luetaan peräkkäisinä
    // klustereina, koska sen taulumerkinnät on tyhjennetty.
    let mode = if entry.deleted {
        None
    } else {
        fat
    };
    write_chain(
        dev,
        info,
        mode,
        entry.first_cluster,
        entry.size as u64,
        &mut out,
    )?;
    out.flush()?;
    Ok((path, mode.is_none()))
}
#[cfg(test)]
mod tests {
    use super::*;

    fn make_lfn(first_byte: u8, checksum: u8, text: &str) -> [u8; 32] {
        let mut e = [0u8; 32];
        e[0] = first_byte;
        e[11] = 0x0F;
        e[13] = checksum;
        let mut units: Vec<u16> = text.chars().map(|c| c as u16).collect();
        if units.len() < 13 {
            units.push(0);
        }
        while units.len() < 13 {
            units.push(0xFFFF);
        }
        units.truncate(13);
        let offsets: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
        for (i, &u) in units.iter().enumerate() {
            let le = u.to_le_bytes();
            e[offsets[i]] = le[0];
            e[offsets[i] + 1] = le[1];
        }
        e
    }

    fn make_sfn(name11: &[u8; 11], deleted: bool, cluster: u32, size: u32) -> [u8; 32] {
        let mut e = [0u8; 32];
        e[0] = if deleted { 0xE5 } else { name11[0] };
        e[1..11].copy_from_slice(&name11[1..11]);
        e[11] = 0x20;
        let c = cluster.to_le_bytes();
        e[26] = c[0];
        e[27] = c[1];
        let s = size.to_le_bytes();
        e[28..32].copy_from_slice(&s);
        e
    }

    fn concat(parts: &[[u8; 32]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn checksum_differs_for_different_names() {
        let a = lfn_checksum(b"PITKAN~1TXT");
        let b = lfn_checksum(b"KOETIE~1TXT");
        assert_ne!(a, b);
        assert_eq!(a, lfn_checksum(b"PITKAN~1TXT"));
    }

    #[test]
    fn parses_lfn_name() {
        let sfn11 = b"PITKAN~1TXT";
        let csum = lfn_checksum(sfn11);
        // Todellinen tallennusjärjestys: korkein order ensin.
        let e2 = make_lfn(0x42, csum, "dosto.txt");
        let e1 = make_lfn(0x41, csum, "pitkanimi_tie");
        let data = concat(&[e2, e1, make_sfn(sfn11, false, 100, 123)]);
        let entries = parse_dir_entries(&data, "");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "pitkanimi_tiedosto.txt");
        assert!(!entries[0].deleted);
        assert_eq!(entries[0].first_cluster, 100);
        assert_eq!(entries[0].size, 123);
    }

    #[test]
    fn deleted_lfn_does_not_pollute_next_file() {
        let dead_sfn = b"VANHAT~1TXT";
        let dead_csum = lfn_checksum(dead_sfn);
        let live_sfn = b"UUSITI~1TXT";
        let live_csum = lfn_checksum(live_sfn);
        // Poistetun LFN-osat: tallennusjärjestys käänteinen, kaikki 0xE5.
        let d2 = make_lfn(0xE5, dead_csum, "o.txt");
        let d1 = make_lfn(0xE5, dead_csum, "vanha_tiedost");
        let dead = make_sfn(dead_sfn, true, 5, 10);
        // Elävän LFN-osat omalla tarkistussummallaan.
        let f2 = make_lfn(0x42, live_csum, "o.txt");
        let f1 = make_lfn(0x41, live_csum, "uusi_tiedost");
        let live = make_sfn(live_sfn, false, 6, 20);
        let data = concat(&[d2, d1, dead, f2, f1, live]);
        let entries = parse_dir_entries(&data, "");
        assert_eq!(entries.len(), 2);
        assert!(entries[0].deleted);
        assert_eq!(entries[0].name, "vanha_tiedosto.txt");
        assert!(!entries[1].deleted);
        assert_eq!(entries[1].name, "uusi_tiedosto.txt");
    }

    #[test]
    fn deleted_lfn_parts_are_reversed() {
        let sfn11 = b"KOETIE~1TXT";
        let csum = lfn_checksum(sfn11);
        // 16 merkin nimi -> 2 osaa; poistettuna molempien order on 0xE5.
        let part2 = make_lfn(0xE5, csum, "txt"); // tallennettu ensin
        let part1 = make_lfn(0xE5, csum, "koe_tiedosto."); // tallennettu viimeksi
        let dead = make_sfn(sfn11, true, 7, 16);
        let data = concat(&[part2, part1, dead]);
        let entries = parse_dir_entries(&data, "");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].deleted);
        assert_eq!(entries[0].name, "koe_tiedosto.txt");
    }

    #[test]
    fn skips_dots_volume_and_end_marker() {
        let mut vol = [0u8; 32];
        vol[0..10].copy_from_slice(b"ASEMABELLI");
        vol[11] = 0x08;
        let dot = make_sfn(b".          ", false, 0, 0);
        let dotdot = make_sfn(b"..         ", false, 0, 0);
        let mut data = concat(&[vol, dot, dotdot, make_sfn(b"TIEDOSTOTXT", false, 9, 1)]);
        data.extend_from_slice(&[0u8; 32]);
        let entries = parse_dir_entries(&data, "");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "TIEDOSTO.TXT");
    }

    #[test]
    fn fat32_high_cluster_bits() {
        let mut e = make_sfn(b"TIEDOSTOTXT", false, 0x0005, 10);
        e[20] = 0x01;
        e[21] = 0x00;
        let data = concat(&[e]);
        let entries = parse_dir_entries(&data, "");
        assert_eq!(entries[0].first_cluster, 0x0001_0005);
    }
}