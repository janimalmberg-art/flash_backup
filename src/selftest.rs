//! Loppuun asti menevät testit: rakennetaan aitoja FAT-levykuvia
//! (fatfs format_volume), kirjoitetaan testitiedostoja, simuloidaan
//! poistoa samoin kuin Windows tekee (0xE5-merkinnät + tyhjennetty
//! FAT-ketju) ja varmistetaan, että syväpalautus ja signatuuripalautus
//! tuottavat alkuperäiset tavut täsmälleen takaisin.
//!
//! Aja komennolla: cargo test

use crate::carve;
use crate::fatinfo::{self, FatType};
use crate::fatwalk;
use crate::rawdev::Device;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::path::PathBuf;

/// Yksilöllinen väliaikainen levykuvapolku (testinimi + prosessitunnus).
fn temp_card(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("muistikortti_palautus_testit");
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join(format!("{}_{}.img", name, std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn pattern(byte: u8, len: usize) -> Vec<u8> {
    vec![byte; len]
}

/// Poistettavan tiediston testidata: selkeä tunniste + täytettä.
fn del_data() -> Vec<u8> {
    let mut v = b"POISTETTUDATA".to_vec();
    v.extend_from_slice(&pattern(0xBB, 3000 - v.len()));
    v
}

/// Aito WAV: RIFF + WAVE + fmt + data. Kokokenttä = pituus - 8.
fn make_wav() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&0u32.to_le_bytes()); // korvataan alla
    v.extend_from_slice(b"WAVE");
    v.extend_from_slice(b"fmt ");
    v.extend_from_slice(&16u32.to_le_bytes()); // chunk-koko
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // kanavat
    v.extend_from_slice(&8000u32.to_le_bytes()); // näytetaajuus
    v.extend_from_slice(&8000u32.to_le_bytes()); // tavua/s
    v.extend_from_slice(&1u16.to_le_bytes()); // blokin koko
    v.extend_from_slice(&8u16.to_le_bytes()); // bittia/näyte
    v.extend_from_slice(b"data");
    v.extend_from_slice(&64u32.to_le_bytes());
    v.extend_from_slice(&pattern(0x55, 64));
    let size = (v.len() - 8) as u32;
    v[4..8].copy_from_slice(&size.to_le_bytes());
    v
}

/// Minimaalinen PNG: allekirjoitus + IHDR + IEND.
fn make_png() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"\x89PNG\r\n\x1A\n");
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0, 8, 0, 0, 0, 0]);
    v.extend_from_slice(&0x374A9C1Bu32.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(b"IEND");
    v.extend_from_slice(&0xAE426082u32.to_be_bytes());
    v
}

/// Minimaalinen JPEG: SOI + JFIF APP0 + COM-segmentti + EOI.
fn make_jpeg() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE0]);
    v.extend_from_slice(&16u16.to_be_bytes()); // APP0-koko
    v.extend_from_slice(b"JFIF\x00");
    v.extend_from_slice(&[0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
    v.extend_from_slice(&[0xFF, 0xFE]); // COM
    let text = b"CARVE_TESTI_JPEG_CARVE_TESTI_JPEG_C";
    v.extend_from_slice(&(text.len() as u16 + 2).to_be_bytes());
    v.extend_from_slice(text);
    v.extend_from_slice(&[0xFF, 0xD9]);
    v
}

/// Minimaalinen ZIP: paikallinen otsikko + data + EOCD.
fn make_zip() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"PK\x03\x04");
    v.extend_from_slice(&[0x14, 0x00]); // versio
    v.extend_from_slice(&[0x00, 0x00]); // liput
    v.extend_from_slice(&[0x00, 0x00]); // pakkaustapa
    v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // aika/pvm
    v.extend_from_slice(&0x12345678u32.to_le_bytes()); // CRC
    v.extend_from_slice(&4u32.to_le_bytes()); // pakattu
    v.extend_from_slice(&4u32.to_le_bytes()); // pakkaamaton
    v.extend_from_slice(&1u16.to_le_bytes()); // nimen pituus
    v.extend_from_slice(&0u16.to_le_bytes()); // extra
    v.extend_from_slice(b"a");
    v.extend_from_slice(b"DATA");
    v.extend_from_slice(b"PK\x05\x06");
    v.extend_from_slice(&[0x00, 0x00]); // levy
    v.extend_from_slice(&[0x00, 0x00]); // cd-levy
    v.extend_from_slice(&1u16.to_le_bytes()); // merkinnat
    v.extend_from_slice(&1u16.to_le_bytes()); // yhteensa
    v.extend_from_slice(&0u32.to_le_bytes()); // cd-koko
    v.extend_from_slice(&0u32.to_le_bytes()); // cd-alku
    v.extend_from_slice(&0u16.to_le_bytes()); // kommentti
    v
}
/// Minimaalinen MP4: ftyp-boxi + moov-boxi.
fn make_mp4() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&16u32.to_be_bytes());
    v.extend_from_slice(b"ftyp");
    v.extend_from_slice(b"isom");
    v.extend_from_slice(&0x200u32.to_be_bytes());
    v.extend_from_slice(&16u32.to_be_bytes());
    v.extend_from_slice(b"moov");
    v.extend_from_slice(b"abcdefgh");
    v
}

/// Minimaalinen BMP (BITMAPINFOHEADER + 12 tavua pikseleita).
fn make_bmp() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"BM");
    v.extend_from_slice(&0u32.to_le_bytes()); // koko: taydennetaan alla
    v.extend_from_slice(&0u32.to_le_bytes()); // varatut
    v.extend_from_slice(&54u32.to_le_bytes()); // pikselidatan alku
    v.extend_from_slice(&40u32.to_le_bytes()); // DIB-otsikon koko
    v.extend_from_slice(&2i32.to_le_bytes()); // leveys
    v.extend_from_slice(&2i32.to_le_bytes()); // korkeus
    v.extend_from_slice(&1u16.to_le_bytes()); // tasot
    v.extend_from_slice(&24u16.to_le_bytes()); // bittia/pikseli
    v.extend_from_slice(&0u32.to_le_bytes()); // pakkaus
    v.extend_from_slice(&12u32.to_le_bytes()); // kuvan koko
    v.extend_from_slice(&0u32.to_le_bytes()); // ppm x
    v.extend_from_slice(&0u32.to_le_bytes()); // ppm y
    v.extend_from_slice(&0u32.to_le_bytes()); // varit
    v.extend_from_slice(&0u32.to_le_bytes()); // tarkeat varit
    v.extend_from_slice(&pattern(0x77, 12));
    let size = v.len() as u32;
    v[2..6].copy_from_slice(&size.to_le_bytes());
    v
}

/// Minimaalinen SQLite-tietokanta: otsikkosivu, 2 x 512 tavua.
fn make_sqlite() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"SQLite format 3\x00"); // 0..16
    v.extend_from_slice(&512u16.to_be_bytes()); // sivukoko @16
    v.extend_from_slice(&[1u8, 1u8, 0, 0, 0, 64, 0, 32, 0, 32]); // versiot ym.
    v.extend_from_slice(&[0u8; 4]); // sivumaara @28, taydennetaan alla
    while v.len() < 1024 {
        v.push(0x00);
    }
    v[28..32].copy_from_slice(&2u32.to_be_bytes());
    v
}
/// Rakentaa FAT32-levykuvan (64 MiB) ja kirjoittaa testitiedostot.
fn make_fat32_card(name: &str) -> (PathBuf, Vec<(String, Vec<u8>)>) {
    use fatfs::{format_volume, FormatVolumeOptions, FileSystem, FsOptions};

    let path = temp_card(name);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .expect("levykuvan luonti");
    format_volume(
        &mut file,
        FormatVolumeOptions::new()
            .total_sectors(131_072)
            .bytes_per_sector(512)
            .bytes_per_cluster(512)
            .fat_type(fatfs::FatType::Fat32)
            .volume_label(*b"TESTIASEMA1"),
    )
    .expect("FAT32-formatointi");
    let files = vec![
        ("elava.txt".to_string(), pattern(0x11, 1500)),
        ("poistettava.txt".to_string(), del_data()),
        ("aani.wav".to_string(), make_wav()),
        ("kuva.png".to_string(), make_png()),
        ("koe.jpg".to_string(), make_jpeg()),
        ("arkisto.zip".to_string(), make_zip()),
        ("elokuva.mp4".to_string(), make_mp4()),
        ("kanta.db".to_string(), make_sqlite()),
        ("luonnos.bmp".to_string(), make_bmp()),
        ("pitkä suomenkielinen tiedosto.txt".to_string(), pattern(0x33, 800)),
    ];
    {
        let fs = FileSystem::new(&mut file, FsOptions::new()).expect("fs auki");
        let root = fs.root_dir();
        for (name, data) in &files {
            let mut f = root.create_file(name).expect("tiedoston luonti");
            f.write_all(data).expect("kirjoitus");
        }
        let sub = root.create_dir("alikansio").expect("alikansion luonti");
        let mut sf = sub.create_file("alatiedosto.txt").expect("alatiedosto");
        sf.write_all(&pattern(0x22, 700)).expect("kirjoitus");
    }
    drop(file);
    (path, files)
}

/// Rakentaa FAT16-levykuvan (4 MiB).
fn make_fat16_card(name: &str) -> (PathBuf, Vec<(String, Vec<u8>)>) {
    use fatfs::{format_volume, FormatVolumeOptions, FileSystem, FsOptions};

    let path = temp_card(name);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .expect("levykuvan luonti");
    format_volume(
        &mut file,
        FormatVolumeOptions::new()
            .total_sectors(8192)
            .bytes_per_sector(512)
            .bytes_per_cluster(512)
            .fat_type(fatfs::FatType::Fat16),
    )
    .expect("FAT16-formatointi");
    let files = vec![
        ("elava.txt".to_string(), pattern(0x11, 500)),
        ("poistettava.txt".to_string(), del_data()),
        ("aani.wav".to_string(), make_wav()),
    ];
    {
        let fs = FileSystem::new(&mut file, FsOptions::new()).expect("fs auki");
        let root = fs.root_dir();
        for (name, data) in &files {
            let mut f = root.create_file(name).expect("tiedoston luonti");
            f.write_all(data).expect("kirjoitus");
        }
    }
    drop(file);
    (path, files)
}
/// Simuloi Windowsin poiston: 8.3-merkinnan eka tavu 0xE5, LFN-osat
/// samoin ja FAT-ketjumerkinnat tyhjiksi.
fn mark_deleted(path: &Path, first_cluster: u32, size: u32, info: &fatinfo::FatInfo) {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("kuva auki muokkaukseen");
    let mut buf = Vec::new();
    f.seek(SeekFrom::Start(0)).unwrap();
    f.read_to_end(&mut buf).unwrap();
    // Etsi 8.3-merkinta klusterin ja koon perusteella (attribuutti 0x20).
    let mut sfn_off = None;
    for i in 0..buf.len() / 32 {
        let o = i * 32;
        let lo = u16::from_le_bytes([buf[o + 26], buf[o + 27]]) as u32;
        // FAT12/16: tavut 20-21 ovat paasmaika. Vain FAT32:ssa niissa on
        // klusterin ylabitit.
        let c = if info.fat_type == FatType::Fat32 {
            lo | ((u16::from_le_bytes([buf[o + 20], buf[o + 21]]) as u32) << 16)
        } else {
            lo
        };
        if c != first_cluster {
            continue;
        }
        let sz = u32::from_le_bytes([buf[o + 28], buf[o + 29], buf[o + 30], buf[o + 31]]);
        // fatfs-rs ei aseta arkistobittia: hyvaksytaan mikka tahansa
        // tiedostoluokka paitsi volyymi (0x08), hakemisto (0x10) ja LFN (0x0F).
        if sz == size && (buf[o + 11] & 0x1E) == 0 {
            sfn_off = Some(o);
            break;
        }
    }
    let o = match sfn_off {
        Some(v) => v,
        None => panic!("poistettavan merkintaa ei loytynyt kuvasta"),
    };
    // Merkitse kaikki LFN-osat (attribuutti 0x0F) heti edessa poistetuiksi.
    let mut back = 1usize;
    while o >= back * 32 && buf[o - back * 32 + 11] == 0x0F {
        buf[o - back * 32] = 0xE5;
        back += 1;
    }
    buf[o] = 0xE5;
    // Tyhjenna FAT-ketjumerkinnat kuten poistossa tapahtuu.
    let entry_size = match info.fat_type {
        FatType::Fat16 => 2u64,
        FatType::Fat32 => 4u64,
        FatType::Fat12 => return, // ei testata taydellisesti
    };
    let fat_base = info.fat_start() * info.bytes_per_sector as u64;
    // Ketjun loppumerkinta riippuu tiedostojärjestelmasta.
    let eoc_min: u32 = match info.fat_type {
        FatType::Fat16 => 0x0000_FFF8,
        FatType::Fat32 => 0x0FFF_FFF8,
        FatType::Fat12 => 0x0000_0FF8,
    };
    let mut c = first_cluster;
    let mut guard = 0u32;
    while c >= 2 && guard < 100_000 {
        let off = (fat_base + c as u64 * entry_size) as usize;
        let size = entry_size as usize;
        if off + size > buf.len() {
            break;
        }
        let next = match info.fat_type {
            FatType::Fat16 => u16::from_le_bytes([buf[off], buf[off + 1]]) as u32,
            FatType::Fat32 => {
                u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
                    & 0x0FFF_FFFF
            }
            FatType::Fat12 => break,
        };
        match info.fat_type {
            FatType::Fat16 => buf[off..off + 2].copy_from_slice(&[0, 0]),
            FatType::Fat32 => buf[off..off + 4].copy_from_slice(&[0; 4]),
            FatType::Fat12 => {}
        }
        if next < 2 || next >= eoc_min {
            break;
        }
        c = next;
        guard += 1;
    }
    f.seek(SeekFrom::Start(0)).unwrap();
    f.write_all(&buf).unwrap();
}
/// Avaa kuvan ja analysoi FAT-rakenteen.
fn analyze(path: &Path) -> (Device, fatinfo::FatInfo, Option<Vec<u8>>) {
    let mut dev = Device::open_image(path).expect("kuvan avaus");
    let info = fatinfo::analyze_device(&mut dev)
        .expect("analyysi epaonnistui")
        .expect("FAT-rakennetta ei loytynyt kuvasta");
    let fat = fatinfo::read_fat_table(&mut dev, &info).expect("FAT-luku");
    (dev, info, fat)
}

/// Varmista tuloskansio ja palauta polku tekstimuodossa.
fn out_base(name: &str) -> String {
    let d = std::env::temp_dir().join(name);
    let _ = std::fs::create_dir_all(&d);
    d.to_str().unwrap().to_string()
}

#[test]
fn fat32_deep_recovery_restores_deleted_files_exactly() {
    let (path, _files) = make_fat32_card("syva32");
    let (mut dev, info, fat) = analyze(&path);
    let entries = fatwalk::scan_tree(&mut dev, &info, fat.as_deref(), 16).unwrap();
    // Elava rakenne: LFN-nimet, 8.3-nimet ja alikansiot loytyvat.
    assert!(entries
        .iter()
        .any(|e| !e.deleted && e.name.eq_ignore_ascii_case("elava.txt")));
    assert!(entries
        .iter()
        .any(|e| !e.deleted && e.name == "pitkä suomenkielinen tiedosto.txt"));
    assert!(entries
        .iter()
        .any(|e| !e.deleted && e.name.eq_ignore_ascii_case("alatiedosto.txt")));

    // Merkitse elava tiedosto poistetuksi kuten Windows tekee.
    let target = entries
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case("poistettava.txt") && !e.deleted)
        .expect("poistettava.txt ei loytynyt elavana")
        .clone();
    assert_eq!(target.size as usize, 3000);
    mark_deleted(&path, target.first_cluster, target.size, &info);

    // Uudelleenavaus: merkinta nayttaa poistuneena ja palautuu taydellisesti.
    let (mut dev2, info2, fat2) = analyze(&path);
    let entries = fatwalk::scan_tree(&mut dev2, &info2, fat2.as_deref(), 16).unwrap();
    let deleted = entries
        .iter()
        .find(|e| e.deleted && e.name.eq_ignore_ascii_case("poistettava.txt"))
        .expect("poistettu merkinta ei nay skannauksessa");
    assert_eq!(deleted.size as usize, 3000);
    let (out, guessed) = fatwalk::recover_entry(
        &mut dev2,
        &info2,
        fat2.as_deref(),
        deleted,
        &out_base("syva32_tulos"),
    )
    .unwrap();
    assert!(guessed, "poistetun ketjun piti luka perakkaisina klustereina");
    assert_eq!(std::fs::read(&out).unwrap(), del_data());

    // Elava tiedosto palautuu taysmalleen FAT-ketjua pitkin.
    let live = entries
        .iter()
        .find(|e| !e.deleted && e.name.eq_ignore_ascii_case("elava.txt"))
        .expect("elava.txt katosi");
    let (out2, guessed2) = fatwalk::recover_entry(
        &mut dev2,
        &info2,
        fat2.as_deref(),
        live,
        &out_base("syva32_tulos"),
    )
    .unwrap();
    assert!(!guessed2);
    assert_eq!(std::fs::read(&out2).unwrap(), pattern(0x11, 1500));
}

#[test]
fn fat32_carving_restores_media_files_exactly() {
    let (path, files) = make_fat32_card("carve32");
    let (mut dev, _info, _fat) = analyze(&path);
    let results = carve::carve_all(&mut dev, &out_base("carve32_tulos")).unwrap();
    assert!(!results.is_empty(), "mitaan ei kaivettu esiin");
    for name in [
        "aani.wav",
        "kuva.png",
        "koe.jpg",
        "arkisto.zip",
        "elokuva.mp4",
        "kanta.db",
        "luonnos.bmp",
    ] {
        let orig = &files.iter().find(|(n, _)| n == name).unwrap().1;
        let ok = results
            .iter()
            .any(|r| std::fs::read(&r.path).map(|d| &d == orig).unwrap_or(false));
        assert!(
            ok,
            "{} ei palautunut tavut tasmalleen ({} tulosta)",
            name,
            results.len()
        );
    }
}
#[test]
fn fat16_deep_recovery_restores_deleted_files_exactly() {
    let (path, _files) = make_fat16_card("syva16");
    let (mut dev, info, fat) = analyze(&path);
    let entries = fatwalk::scan_tree(&mut dev, &info, fat.as_deref(), 16).unwrap();
    let target = entries
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case("poistettava.txt") && !e.deleted)
        .expect("poistettava.txt ei loytynyt")
        .clone();
    mark_deleted(&path, target.first_cluster, target.size, &info);

    let (mut dev2, info2, fat2) = analyze(&path);
    let entries = fatwalk::scan_tree(&mut dev2, &info2, fat2.as_deref(), 16).unwrap();
    let deleted = entries
        .iter()
        .find(|e| e.deleted && e.name.eq_ignore_ascii_case("poistettava.txt"))
        .expect("poistettu merkinta ei nay FAT16-skannauksessa");
    let (out, guessed) = fatwalk::recover_entry(
        &mut dev2,
        &info2,
        fat2.as_deref(),
        deleted,
        &out_base("syva16_tulos"),
    )
    .unwrap();
    assert!(guessed);
    assert_eq!(std::fs::read(&out).unwrap(), del_data());
}

#[test]
fn carving_rejects_false_positives() {
    // Raaka kuva ilman tiedostojarjestelmaa: valeosumat, jotka aiempi
    // versio olisi palauttanut vaarin.
    let path = temp_card("epailyttava");
    let mut data = pattern(0x00, 1024 * 1024);
    data[..2].copy_from_slice(b"BM");
    data[2..6].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // kelvoton koko
    data[64..67].copy_from_slice(&[0xFF, 0xD8, 0xFF]);
    data[67] = 0x00; // segmenttimerkki ei-kelvollinen
    data[128..132].copy_from_slice(b"RIFF");
    data[132..136].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // yli max-rajan
    data[136..140].copy_from_slice(b"ABCD");
    data[200..216].copy_from_slice(b"SQLite format 3\x00");
    data[216..218].copy_from_slice(&3u16.to_be_bytes()); // ei kahden potenssi
    std::fs::write(&path, &data).unwrap();

    let mut dev = Device::open_image(&path).unwrap();
    let results = carve::carve_all(&mut dev, &out_base("fp_tulos")).unwrap();
    assert!(
        results.is_empty(),
        "vaarat osumat pitaisi suodattaa: {:?}",
        results.iter().map(|r| r.kind.clone()).collect::<Vec<_>>()
    );
}
