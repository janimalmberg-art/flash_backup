//! Ohjattu sektoriselaus: hexdump, selaus, haku ja tallennus.

use crate::rawdev::{find_all, hexdump, human, out_path, Device, SECTOR};
use dialoguer::{theme::ColorfulTheme, Input, Select};
use std::fs::File;
use std::io::{self, Write};

/// Keskustelee käyttäjän kanssa ja selailee sektoreita yksi lohko kerrallaan.
pub fn run_browser(dev: &mut Device) -> Result<(), Box<dyn std::error::Error>> {
    println!();
    println!("=== OHJATTU SEKTORISELAUS ===");
    println!(
        "Selaa muistikortin muistilohkoja sektori kerrallaan ({} tavua/sektori).",
        SECTOR
    );
    match dev.total_sectors() {
        Some(n) => println!("Kortilla {} sektoria ({}).", n, human(n * SECTOR)),
        None => println!("Kortin kokoa ei tiedetä; lukujen loppu havaitaan automaattisesti."),
    }

    let menu = [
        "Seuraava sektori (+1)",
        "Edellinen sektori (-1)",
        "Siirry 16 sektoria eteenpäin",
        "Siirry 16 sektoria taaksepäin",
        "Hyppää sektorinumeroon...",
        "Tallenna tämä sektori tiedostoon",
        "Tallenna useita sektoreita tiedostoon...",
        "Etsi tekstiä eteenpäin...",
        "Etsi heksatavuja eteenpäin...",
        "Takaisin päävalikkoon",
    ];
    let mut sector: u64 = 0;
    loop {
        let buf = dev.read_sectors(sector, 1)?;
        println!();
        if buf.is_empty() {
            println!("Sektori {}: ei dataa (laitteen loppu).", sector);
            if sector > 0 {
                sector -= 1;
            }
            continue;
        }
        println!("--- Sektori {} (tavuoffset {:#X}) ---", sector, sector * SECTOR);
        hexdump(&buf, sector * SECTOR);
        let sel = Select::with_theme(&ColorfulTheme::default())
            .with_prompt(format!("Sektori {} - valitse toiminto", sector))
            .items(&menu)
            .default(0)
            .max_length(10)
            .interact()?;
        match sel {
            0 => sector += 1,
            1 => sector = sector.saturating_sub(1),
            2 => sector += 16,
            3 => sector = sector.saturating_sub(16),
            4 => {
                let limit = dev.total_sectors().unwrap_or(u64::MAX).saturating_sub(1);
                let s = Input::<String>::new()
                    .with_prompt(format!("Sektorinumero (0-{})", limit))
                    .interact()?;
                match s.trim().parse::<u64>() {
                    Ok(n) if n <= limit => sector = n,
                    Ok(_) => println!("Numero on kortin ulkopuolella."),
                    Err(_) => println!("Ei kelvollinen numero."),
                }
            }
            5 => {
                save_range(dev, sector, 1)?;
            }
            6 => {
                let s = Input::<String>::new()
                    .with_prompt("Kuinka monta sektoria tallennetaan? (esim. 8, 64, 2048)")
                    .interact()?;
                match s.trim().parse::<u64>() {
                    Ok(n) if n > 0 => save_range(dev, sector, n)?,
                    _ => println!("Ei kelvollinen määrä."),
                }
            }
            7 => {
                let text = Input::<String>::new()
                    .with_prompt("Etsittävä teksti")
                    .interact()?;
                if !text.is_empty() {
                    // Haetaan sekä tavumuoto että UTF-16LE (Windows-nimet, FAT LFN).
                    let mut needles = vec![text.clone().into_bytes()];
                    let mut u16le = Vec::new();
                    for c in text.encode_utf16() {
                        u16le.extend_from_slice(&c.to_le_bytes());
                    }
                    needles.push(u16le);
                    match search_forward(dev, (sector + 1) * SECTOR, &needles)? {
                        Some((abs, ni)) => {
                            let kind = if ni == 0 { "ASCII/UTF-8" } else { "UTF-16LE" };
                            println!(
                                "Löytyi ({}) tavuoffsetista {:#X} (sektori {}).",
                                kind,
                                abs,
                                abs / SECTOR
                            );
                            sector = abs / SECTOR;
                        }
                        None => println!("Ei löytynyt kortin loppuun asti."),
                    }
                }
            }
            8 => {
                let text = Input::<String>::new()
                    .with_prompt("Etsittävä heksajono (esim. FFD8FF)")
                    .interact()?;
                let clean: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
                if !clean.is_empty() && clean.len() % 2 == 0 {
                    let bytes: Vec<u8> = (0..clean.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).unwrap())
                        .collect();
                    match search_forward(dev, (sector + 1) * SECTOR, &[bytes])? {
                        Some((abs, _)) => {
                            println!(
                                "Löytyi tavuoffsetista {:#X} (sektori {}).",
                                abs,
                                abs / SECTOR
                            );
                            sector = abs / SECTOR;
                        }
                        None => println!("Ei löytynyt kortin loppuun asti."),
                    }
                } else {
                    println!("Heksojen pituuden pitää olla parillinen ja nollasta poikkeava.");
                }
            }
            _ => break,
        }
    }
    Ok(())
}

/// Tallentaa sektorialueen tiedostoon kansioon `palautetut`.
fn save_range(dev: &mut Device, start: u64, count: u64) -> io::Result<()> {
    let name = format!("sektori_{}_{}.bin", start, count);
    let path = out_path("palautetut", &name);
    let mut out = File::create(&path)?;
    let batch = 2048u64; // 1 MiB
    let mut saved = 0u64;
    while saved < count {
        let n = batch.min(count - saved);
        let data = dev.read_sectors(start + saved, n)?;
        if data.is_empty() {
            println!(
                "Laitteen loppu kohdattiin ({} sektoria tallennettu).",
                saved
            );
            break;
        }
        out.write_all(&data)?;
        saved += n;
    }
    println!(
        "-> Tallennettu: {} ({} sektoria, {})",
        path.display(),
        saved,
        human(saved * SECTOR)
    );
    Ok(())
}

/// Etsii jonkin hakuehdoista eteenpäin annetusta tavuoffsetista.
/// Palauttaa (absoluuttinen offset, haun indeksi), jos löytyi.
fn search_forward(
    dev: &mut Device,
    start_byte: u64,
    needles: &[Vec<u8>],
) -> io::Result<Option<(u64, usize)>> {
    let chunk = 512 * 1024;
    let mut pos = start_byte;
    let max_needle = needles.iter().map(|n| n.len()).max().unwrap_or(1);
    loop {
        let buf = dev.read_at(pos, chunk)?;
        if buf.is_empty() {
            return Ok(None);
        }
        for (ni, needle) in needles.iter().enumerate() {
            if let Some(&first) = find_all(&buf, needle, 1).first() {
                return Ok(Some((pos + first as u64, ni)));
            }
        }
        if buf.len() < chunk {
            return Ok(None);
        }
        // Siirrytään eteenpäin pienellä päällekkäisyydellä reunatapausten varalta.
        pos += buf.len() as u64 - max_needle.min(64) as u64 + 1;
    }
}