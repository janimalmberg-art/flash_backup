//! Muistikortin syväpalautustyökalu.
//!
//! Lukee muistikortin raakatavuina (vaatii järjestelmänvalvojan oikeudet)
//! ja tarjoaa ohjatut työkalut palautettavan datan etsimiseen lohkoilta ja
//! sektoreilta myös silloin, kun kortti on formatoitu. Kortille EI kirjoiteta.

mod browser;
mod carve;
mod fatinfo;
mod fatwalk;
mod rawdev;
#[cfg(test)]
mod selftest;

use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};
use fatinfo::FatInfo;
use rawdev::{human, Device};
use std::fs::File;
use std::io::{Read, Write};

/// UI- ja laitevirheet yhdessä: dialoguerin ja I/O:n virheet kummatkin
/// konvertoituvat tähän automaattisesti `?`-operaattorilla.
type AppResult<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> AppResult<()> {
    if let Err(e) = run() {
        let msg = e.to_string();
        if msg.contains("not a terminal") || msg.contains("is not a tty") {
            println!();
            println!("HUOM: Tämä on vuorovaikutteinen ohjelma - aja se oikeassa komentorivi-");
            println!("ikkunassa (esim. Windows Terminal, cmd.exe tai PowerShell) järjestelmän-");
            println!("valvojana. Putkitettu syöte (pipe) ei toimi valikoiden kanssa.");
            return Ok(());
        }
        return Err(e);
    }
    Ok(())
}

fn run() -> AppResult<()> {
    // Komentoriviargumentti: ajaa koko palautusputken ilman valikoita.
    if let Some(arg) = std::env::args().nth(1) {
        return batch_recover(&arg);
    }
    // Vuorovaikutteinen valikko vaatii oikean terminaalin (TTY).
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        println!();
        println!("HUOM: Tämä on vuorovaikutteinen ohjelma. Aja se oikeassa komentorivi-");
        println!("ikkunassa (esim. Windows Terminal, cmd.exe tai PowerShell), järjestelmän-");
        println!("valvojana, niin valikot toimivat.");
        return Ok(());
    }

    println!("=== MUISTIKORTIN SYVÄPALAUTUS (raakaluku, vain luku) ===");
    println!();
    println!("Tämä työkalu lukee muistikortin sektoreittain raakadatana eikä kirjoita");
    println!("kortille mitään. Palautetut tiedostot tallennetaan työhakemistoon kansion");
    println!("'palautetut' alle. Käynnistä komentorivi JÄRJESTELMÄNVALVOJANA.");
    println!();

    let drive = ask_drive()?;
    let mut dev = match Device::open_any(&drive) {
        Ok(d) => d,
        Err(e) => {
            println!();
            println!("Virhe laitteen avauksessa: {}", e);
            println!("Varmista, että:");
            println!("  1. komentorivi on käynnistetty järjestelmänvalvojana,");
            println!("  2. asema {} on olemassa (esim. muistikortin lukija).", drive);
            return Ok(());
        }
    };
    match dev.size {
        Some(s) => println!("Avattu: {} (koko: {})", dev.path, human(s)),
        None => println!(
            "Avattu: {} (kokoa ei voitu selvittää - lukujen loppu havaitaan automaattisesti)",
            dev.path
        ),
    }

    let mut fat_info: Option<FatInfo> = None;
    loop {
        println!();
        let items = vec![
            "Analysoi tiedostojärjestelmä (MBR / boot-sektori)",
            "Perinteinen palautus (FAT-kirjasto, juurihakemiston tiedostot)",
            "Syväpalautus: elävät JA poistetut tiedostot, myös alihakemistoista",
            "Signatuuriskannaus (carving) - toimii myös formatoituun korttiin",
            "Ohjattu sektoriselaus (hexdump, haku, tallennus)",
            "FAT-taulujen analyysi ja klusteriketjut",
            "Lopeta",
        ];
        let choice = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Mitä haluat tehdä?")
            .default(0)
            .items(&items)
            .max_length(10)
            .interact()?;
        match choice {
            0 => {
                fat_info = fatinfo::analyze_device(&mut dev)?;
            }
            1 => {
                if let Err(e) = classic_fatfs_recover(&drive) {
                    println!();
                    println!("Perinteinen palautus epäonnistui: {}", e);
                    println!("Vinkki: jos kortti on formatoitu tai tiedostojärjestelmä on vioittunut,");
                    println!("käytä kohtaa 'Syväpalautus' tai 'Signatuuriskannaus'.");
                }
            }
            2 => deep_recover(&mut dev, &mut fat_info)?,
            3 => carve::run_carve(&mut dev)?,
            4 => browser::run_browser(&mut dev)?,
            5 => fat_tables_ui(&mut dev, &mut fat_info)?,
            _ => {
                println!("Suljetaan ohjelma.");
                break;
            }
        }
    }
    Ok(())
}

/// Kysyy asematunnusten ja avaa laitteen. Palauttaa isolla kirjoitetun tunnuksen.
fn ask_drive() -> AppResult<String> {
    let s = Input::<String>::new()
        .with_prompt(
            "Syötä aseman tunnus (esim. D), fyysinen levy (PHYSICALDRIVE1) tai levykuvan polku",
        )
        .interact()?;
    let drive = s.trim().to_uppercase();
    if drive.is_empty() {
        println!("Virhe: asematunnus ei voi olla tyhjä.");
        std::process::exit(1);
    }
    Ok(drive)
}

/// Kysyy kokonaislukua oletusarvolla (tyhjä syöte = oletus).
fn ask_u64(prompt: &str, default: u64) -> u64 {
    loop {
        let s = Input::<String>::new()
            .with_prompt(format!("{} (tyhjä = {})", prompt, default))
            .allow_empty(true)
            .interact()
            .unwrap_or_default();
        let t = s.trim().to_string();
        if t.is_empty() {
            return default;
        }
        if let Ok(v) = t.parse::<u64>() {
            return v;
        }
        println!("Syötä kokonaisluku.");
    }
}

/// Alkuperäinen toimintatapa: fatfs-kirjasto listaa juurihakemiston tiedostot.
/// Toimii, jos FAT-tiedostojärjestelmä on ehjä.
fn classic_fatfs_recover(spec: &str) -> AppResult<()> {
    use fatfs::{FileSystem, FsOptions};

    // Tukee asematunnuksen, fyysistä levyä ja levykuvatiedostoa.
    let source = rawdev::Device::open_any(spec)?;
    let device_file = source.detach();
    println!();
    println!("Tutkitaan tiedostojärjestelmää fatfs-kirjastolla...");
    let fs = FileSystem::new(device_file, FsOptions::new())?;
    let root_dir = fs.root_dir();

    let mut files_found = Vec::new();
    for entry_res in root_dir.iter() {
        let entry = entry_res?;
        if entry.is_file() {
            files_found.push(entry.file_name());
        }
    }
    if files_found.is_empty() {
        println!("Juurihakemistosta ei löytynyt tavallisia tiedostoja.");
        println!("Vinkki: jos kortti on formatoitu, kokeile syväpalautusta tai signatuuriskannausta.");
        return Ok(());
    }
    files_found.push("Lopeta tämän toiminnon".to_string());

    loop {
        let selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Valitse palautettava tiedosto")
            .default(0)
            .items(&files_found)
            .max_length(20)
            .interact()?;
        if selection == files_found.len() - 1 {
            break;
        }
        let selected_filename = &files_found[selection];
        println!();
        println!("Palautetaan tiedostoa: {}...", selected_filename);
        let mut source_file = root_dir.open_file(selected_filename)?;
        let safe_name = rawdev::sanitize(selected_filename);
        let target = rawdev::out_path("palautetut", &safe_name);
        let mut target_file = File::create(&target)?;
        let mut buffer = Vec::new();
        source_file.read_to_end(&mut buffer)?;
        target_file.write_all(&buffer)?;
        println!(
            "-> Tiedosto palautettu nimellä: {} ({})",
            target.display(),
            human(buffer.len() as u64)
        );
    }
    Ok(())
}

/// Syväpalautus: elävät + poistetut tiedostot FAT-hakemistopuusta.
fn deep_recover(dev: &mut Device, fat_info: &mut Option<FatInfo>) -> AppResult<()> {
    if fat_info.is_none() {
        println!();
        println!("Tiedostojärjestelmää ei ole vielä analysoitu - analysoidaan...");
        *fat_info = fatinfo::analyze_device(dev)?;
    }
    let info = match fat_info {
        Some(i) => *i,
        None => {
            println!();
            println!("FAT-rakennetta ei saatu selville automaattisesti.");
            let ok = Confirm::new()
                .with_prompt("Syötetäänkö rakennetiedot manuaalisesti ohjatusti?")
                .default(false)
                .interact()?;
            if !ok {
                return Ok(());
            }
            match manual_fat_info(dev)? {
                Some(i) => {
                    *fat_info = Some(i);
                    i
                }
                None => return Ok(()),
            }
        }
    };
    println!();
    println!("Luetaan FAT-taulu ja skannataan hakemistopuu (elävät + poistetut merkinnät)...");
    let fat = fatinfo::read_fat_table(dev, &info)?;
    let entries = fatwalk::scan_tree(dev, &info, fat.as_deref(), 16)?;
    if entries.is_empty() {
        println!("Hakemistomerkintöjä ei löytynyt. Kokeile signatuuriskannausta.");
        return Ok(());
    }
    let deleted_count = entries.iter().filter(|e| e.deleted).count();
    println!(
        "Löytyi {} kohdetta, joista {} poistettua.",
        entries.len(),
        deleted_count
    );

    loop {
        let mut items: Vec<String> = entries
            .iter()
            .map(|e| {
                let kind = if e.is_dir {
                    "hakemisto".to_string()
                } else {
                    human(e.size as u64)
                };
                format!(
                    "{}{} ({}, klusteri {})",
                    if e.deleted { "[POISTETTU] " } else { "" },
                    e.path,
                    kind,
                    e.first_cluster
                )
            })
            .collect();
        items.push("Takaisin päävalikkoon".to_string());
        let sel = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Valitse palautettava kohde")
            .items(&items)
            .max_length(20)
            .interact()?;
        if sel == items.len() - 1 {
            break;
        }
        let e = &entries[sel];
        if e.is_dir {
            println!("Hakemistoa ei voi palauttaa suoraan - valitse listasta sen tiedostoja.");
            continue;
        }
        match fatwalk::recover_entry(dev, &info, fat.as_deref(), e, "palautetut") {
            Ok((path, guessed)) => {
                println!("-> Palautettu: {}", path.display());
                if guessed {
                    println!("   HUOM: FAT-ketju puuttui, data luettiin peräkkäisinä klustereina.");
                    println!("   Data voi olla pirstaleista, jos tiedosto on tallennettu paloittain.");
                }
            }
            Err(err) => println!("Palautus epäonnistui: {}", err),
        }
    }
    Ok(())
}

/// Ohjattu manuaalinen FAT-rakenteen syöttö (esim. kun VBR on vioittunut).
fn manual_fat_info(dev: &mut Device) -> AppResult<Option<FatInfo>> {
    use fatinfo::FatType;
    println!();
    println!("Ohjattu rakenneasetus (Enter hyväksyy oletusarvon).");
    let bps = ask_u64("Sektorin koko tavuina (tyypillisesti 512)", 512) as u16;
    let spc = ask_u64("Sektoreita per klusteri (tyypillisesti 8)", 8) as u8;
    let reserved = ask_u64(
        "Varattujen sektoreiden määrä (FAT32: tyypillisesti 32)",
        32,
    ) as u16;
    let fats = ask_u64("FAT-taulujen määrä (tyypillisesti 2)", 2) as u8;
    let fat_size_in = ask_u64(
        "Yhden FAT-taulun koko sektoreina (0 = arvaus kortin koon perusteella)",
        0,
    );
    let total_sectors = dev.size.unwrap_or(0) / bps as u64;
    let fat_size = if fat_size_in > 0 {
        fat_size_in as u32
    } else if total_sectors > 0 {
        (total_sectors / 512).max(1) as u32
    } else {
        10_000
    };
    let info = FatInfo {
        vbr_sector: 0,
        bytes_per_sector: bps,
        sectors_per_cluster: spc,
        reserved_sectors: reserved,
        num_fats: fats,
        root_entries: 0,
        total_sectors,
        fat_size_sectors: fat_size,
        root_cluster: 2,
        fat_type: FatType::Fat32,
    };
    println!("Arvio: data-alue alkaa sektorista {}.", info.data_start());
    Ok(Some(info))
}

/// FAT-taulujen analyysi ja klusteriketjut.
fn fat_tables_ui(dev: &mut Device, fat_info: &mut Option<FatInfo>) -> AppResult<()> {
    if fat_info.is_none() {
        println!();
        println!("Analysoidaan ensin tiedostojärjestelmä...");
        *fat_info = fatinfo::analyze_device(dev)?;
    }
    let info = match fat_info {
        Some(i) => *i,
        None => {
            println!("FAT-rakennetta ei saatu selville - FAT-tauluja ei voi analysoida.");
            return Ok(());
        }
    };
    println!();
    println!("{}", info.describe());
    let menu = [
        "Näytä FAT-merkintöjä",
        "Muodosta ja tutki klusteriketju",
        "Klusteritilastot",
        "Takaisin päävalikkoon",
    ];
    loop {
        println!();
        let sel = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("FAT-työkalut")
            .items(&menu)
            .default(0)
            .interact()?;
        match sel {
            0 => {
                let fat = match fatinfo::read_fat_table(dev, &info)? {
                    Some(f) => f,
                    None => continue,
                };
                let n = ask_u64("Kuinka monta merkintää näytetään?", 64) as u32;
                let max = fatinfo::fat_entry_count(fat.len(), info.fat_type);
                let n = n.min(max);
                println!();
                println!("{:>9}  {:>10}  merkitys", "klusteri", "arvo");
                for c in 2..n {
                    let v = fatinfo::fat_entry(&fat, c, info.fat_type);
                    let mean = if v == 0 {
                        "vapaa"
                    } else if v >= fatinfo::eoc(info.fat_type) {
                        "ketjun loppu (EOC)"
                    } else if v == fatinfo::bad(info.fat_type) {
                        "huono (BAD)"
                    } else {
                        "käytössä"
                    };
                    println!("{:>9}  {:#010X}  {}", c, v, mean);
                }
            }
            1 => {
                let fat = match fatinfo::read_fat_table(dev, &info)? {
                    Some(f) => f,
                    None => continue,
                };
                let c = ask_u64("Klusterinumero, josta ketju alkaa", 2) as u32;
                let chain = fatinfo::cluster_chain(&fat, info.fat_type, c);
                if chain.is_empty() {
                    println!("Ketjua ei muodostunut (klusteri ehkä vapaa).");
                    continue;
                }
                println!(
                    "Ketjussa {} klusteria (~{}).",
                    chain.len(),
                    human(chain.len() as u64 * info.cluster_size() as u64)
                );
                for (i, cc) in chain.iter().take(20).enumerate() {
                    println!(
                        "  [{}] klusteri {} -> sektori {}",
                        i,
                        cc,
                        info.lba_of_cluster(*cc)
                    );
                }
                if chain.len() > 20 {
                    println!("  ... ja {} klusteria lisää", chain.len() - 20);
                }
                let ok = Confirm::new()
                    .with_prompt("Tallennetaanko ketjun sisältö tiedostoon?")
                    .default(false)
                    .interact()?;
                if ok {
                    let cap = (chain.len() as u64)
                        .saturating_mul(info.cluster_size() as u64)
                        .min(512 * 1024 * 1024);
                    let data = fatwalk::read_chain(dev, &info, Some(&fat), c, cap)?;
                    let path = rawdev::out_path(
                        "palautetut",
                        &format!("klusteriketju_{}.bin", c),
                    );
                    std::fs::write(&path, &data)?;
                    println!("-> {} ({})", path.display(), human(data.len() as u64));
                }
            }
            2 => {
                let fat = match fatinfo::read_fat_table(dev, &info)? {
                    Some(f) => f,
                    None => continue,
                };
                let mut free = 0u64;
                let mut eoc = 0u64;
                let mut bad = 0u64;
                let mut used = 0u64;
                let max = fatinfo::fat_entry_count(fat.len(), info.fat_type);
                for c in 2..max {
                    let v = fatinfo::fat_entry(&fat, c, info.fat_type);
                    if v == 0 {
                        free += 1;
                    } else if v >= fatinfo::eoc(info.fat_type) {
                        eoc += 1;
                    } else if v == fatinfo::bad(info.fat_type) {
                        bad += 1;
                    } else {
                        used += 1;
                    }
                }
                println!();
                println!(
                    "Klusterit: {} käytössä, {} vapaata, {} EOC-merkintää, {} huonoa.",
                    used, free, eoc, bad
                );
                println!(
                    "Vapaata data-aluetta: ~{}.",
                    human(free * info.cluster_size() as u64)
                );
            }
            _ => break,
        }
    }
    Ok(())
}
/// Komentorivipakettitila: analysoi lähteen, palauttaa poistetut tiedostot
/// ja ajaa signatuuriskannauksen ilman valikoita. Käyttö:
/// muistikortti_palautus.exe <asematunnus|PHYSICALDRIVE1|levykuvan.polku>
fn batch_recover(spec: &str) -> AppResult<()> {
    println!("=== MUISTIKORTIN SYVAPALAUTUS (pakettitila) ===");
    println!("Lähde: {}", spec);
    let mut dev = match rawdev::Device::open_any(spec) {
        Ok(d) => d,
        Err(e) => {
            println!("Virhe lähteen avauksessa: {}", e);
            println!("Käyttö: asematunnus (esim. D), PHYSICALDRIVE1 tai levykuvan polku.");
            return Ok(());
        }
    };
    match dev.size {
        Some(s) => println!("Koko: {}", human(s)),
        None => println!("Kokoa ei saatu selvitettyä."),
    }
    let info = match fatinfo::analyze_device(&mut dev)? {
        Some(i) => i,
        None => {
            println!("FAT-rakennetta ei löytynyt - jatketaan signatuuriskannauksella.");
            println!();
            carve::run_carve(&mut dev)?;
            return Ok(());
        }
    };
    println!("{}", info.describe());
    let fat = fatinfo::read_fat_table(&mut dev, &info)?;
    println!();
    println!("--- Syväpalautus: poistetut tiedostot ---");
    let entries = fatwalk::scan_tree(&mut dev, &info, fat.as_deref(), 16)?;
    let mut files = 0usize;
    for e in entries.iter().filter(|e| e.deleted && !e.is_dir) {
        match fatwalk::recover_entry(&mut dev, &info, fat.as_deref(), e, "palautetut") {
            Ok((path, guessed)) => {
                files += 1;
                println!("-> {}", path.display());
                if guessed {
                    println!("   (data luettiin peräkkäisinä klustereina)");
                }
            }
            Err(err) => println!("   virhe: {}", err),
        }
    }
    println!("Palautettu {} poistettua tiedostoa 'palautetut'-kansioon.", files);
    println!();
    println!("--- Signatuuripalautus ---");
    carve::run_carve(&mut dev)?;
    Ok(())
}
