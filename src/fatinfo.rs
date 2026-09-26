//! MBR:n, FAT-boot-sektorin (VBR) ja FAT-taulujen analysointi.
//!
//! Sektoritason rakenne saadaan selville myös silloin, kun kortti on formatoitu:
//! analyysi perustuu suoraan sektorien sisältöön, ei tiedostojärjestelmään.

use crate::rawdev::Device;
use std::io;

/// Tunnistetut FAT-variantit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

impl FatType {
    pub fn label(self) -> &'static str {
        match self {
            FatType::Fat12 => "FAT12",
            FatType::Fat16 => "FAT16",
            FatType::Fat32 => "FAT32",
        }
    }
}

/// MBR:n partitionmerkintä.
#[derive(Clone, Copy, Debug)]
pub struct Partition {
    pub index: usize,
    pub bootable: bool,
    pub ptype: u8,
    pub lba: u64,
    pub sectors: u64,
}

/// FAT-boot-sektorista (VBR) jäsennetty rakennekuvaus.
#[derive(Clone, Copy, Debug)]
pub struct FatInfo {
    /// Sektori, jolla VBR sijaitsee (MBR-levyllä = osion LBA).
    pub vbr_sector: u64,
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub num_fats: u8,
    pub root_entries: u16,
    pub total_sectors: u64,
    pub fat_size_sectors: u32,
    /// Vain FAT32: juurihakemiston alkuklusteri.
    pub root_cluster: u32,
    pub fat_type: FatType,
}

impl FatInfo {
    /// Klusterin koko tavuina.
    pub fn cluster_size(&self) -> u32 {
        self.bytes_per_sector as u32 * self.sectors_per_cluster as u32
    }

    /// FAT-alueen alku (sektori).
    pub fn fat_start(&self) -> u64 {
        self.vbr_sector + self.reserved_sectors as u64
    }

    /// Data-alueen (klusteri 2) alku (sektori).
    pub fn data_start(&self) -> u64 {
        let root_dir_sectors =
            ((self.root_entries as u64 * 32) + self.bytes_per_sector as u64 - 1)
                / self.bytes_per_sector as u64;
        self.fat_start()
            + self.num_fats as u64 * self.fat_size_sectors as u64
            + root_dir_sectors
    }

    /// Klusterin ensimmäinen sektori.
    pub fn lba_of_cluster(&self, cluster: u32) -> u64 {
        self.data_start() + (cluster as u64 - 2) * self.sectors_per_cluster as u64
    }

    /// Ihmisluettava yhteenveto.
    pub fn describe(&self) -> String {
        format!(
            "Tyyppi: {} | sektori: {} B | klusteri: {} B ({} sekt.) | varattu: {} sekt. | FAT:ia: {} x {} sekt. | juurihakemisto: {} merk.{} | koko: {} sekt. (~{:.1} MiB)",
            self.fat_type.label(),
            self.bytes_per_sector,
            self.cluster_size(),
            self.sectors_per_cluster,
            self.reserved_sectors,
            self.num_fats,
            self.fat_size_sectors,
            self.root_entries,
            if self.fat_type == FatType::Fat32 {
                format!(" (klusteri {})", self.root_cluster)
            } else {
                String::new()
            },
            self.total_sectors,
            self.total_sectors as f64 * self.bytes_per_sector as f64 / (1024.0 * 1024.0),
        )
    }
}

/// Jäsentää MBR-partition taulun. Palauttaa tyhjän listan, jos allekirjoitusta ei ole.
pub fn parse_mbr(buf: &[u8]) -> Vec<Partition> {
    let mut out = Vec::new();
    if buf.len() < 512 || buf[510] != 0x55 || buf[511] != 0xAA {
        return out;
    }
    for i in 0..4 {
        let e = &buf[446 + i * 16..446 + (i + 1) * 16];
        let ptype = e[4];
        let lba = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as u64;
        let sectors = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as u64;
        if ptype == 0 || sectors == 0 {
            continue;
        }
        out.push(Partition {
            index: i,
            bootable: e[0] & 0x80 != 0,
            ptype,
            lba,
            sectors,
        });
    }
    out
}

/// Ihmisluettava osiotyyppi.
pub fn partition_type_name(ptype: u8) -> &'static str {
    match ptype {
        0x01 => "FAT12",
        0x04 | 0x06 | 0x0E => "FAT16",
        0x05 | 0x0F => "laajennettu osio",
        0x07 => "NTFS/exFAT",
        0x0B | 0x0C => "FAT32",
        0x83 => "Linux",
        0xEE => "GPT-suojaus",
        _ => "tuntematon",
    }
}

/// Yrittää jäsentää sektorin FAT-boot-sektorina (VBR).
pub fn parse_fat_vbr(buf: &[u8], vbr_sector: u64) -> Option<FatInfo> {
    if buf.len() < 512 || buf[510] != 0x55 || buf[511] != 0xAA {
        return None;
    }
    let bps = u16::from_le_bytes([buf[11], buf[12]]);
    if !matches!(bps, 512 | 1024 | 2048 | 4096) {
        return None;
    }
    let spc = buf[13];
    if spc == 0 || !spc.is_power_of_two() {
        return None;
    }
    let reserved = u16::from_le_bytes([buf[14], buf[15]]);
    let num_fats = buf[16];
    if num_fats == 0 || num_fats > 4 {
        return None;
    }
    let root_entries = u16::from_le_bytes([buf[17], buf[18]]);
    let total16 = u16::from_le_bytes([buf[19], buf[20]]) as u64;
    let fat_size16 = u16::from_le_bytes([buf[22], buf[23]]) as u32;
    let total32 = u32::from_le_bytes([buf[32], buf[33], buf[34], buf[35]]) as u64;
    let fat_size32 = u32::from_le_bytes([buf[36], buf[37], buf[38], buf[39]]);
    let root_cluster = u32::from_le_bytes([buf[44], buf[45], buf[46], buf[47]]);
    let total = if total16 != 0 { total16 } else { total32 };
    if total == 0 {
        return None;
    }
    let (fat_type, fat_size) = if fat_size16 != 0 {
        // FAT12/FAT16: eritellään klusterien määrän perusteella
        let root_dir_sectors = (root_entries as u32 * 32 + bps as u32 - 1) / bps as u32;
        let data_sectors = total as u32
            - reserved as u32
            - num_fats as u32 * fat_size16
            - root_dir_sectors;
        let clusters = data_sectors / spc as u32;
        let t = if clusters < 4085 {
            FatType::Fat12
        } else {
            FatType::Fat16
        };
        (t, fat_size16)
    } else if fat_size32 != 0 {
        (FatType::Fat32, fat_size32)
    } else {
        return None;
    };
    // Varmistus: "FAT"-merkkijono jossakin tunnetussa kohdassa
    let is_fat = [3usize, 54, 82]
        .iter()
        .any(|&o| buf.get(o..).map(|s| s.starts_with(b"FAT")).unwrap_or(false));
    if !is_fat {
        return None;
    }
    Some(FatInfo {
        vbr_sector,
        bytes_per_sector: bps,
        sectors_per_cluster: spc,
        reserved_sectors: reserved,
        num_fats,
        root_entries,
        total_sectors: total,
        fat_size_sectors: fat_size,
        root_cluster,
        fat_type,
    })
}

/// Tunnistaa sektorin tiedostojärjestelmän karkeasti raporttia varten.
pub fn detect_fs(buf: &[u8]) -> &'static str {
    if buf.len() >= 11 && buf[3..9] == *b"EXFAT " {
        "exFAT"
    } else if buf.len() >= 5
        && [3usize, 54, 82]
            .iter()
            .any(|&o| buf.get(o..).map(|s| s.starts_with(b"FAT")).unwrap_or(false))
    {
        "FAT (boot-sektori)"
    } else if buf.len() >= 6 && buf[0..5] == *b"NTFS " {
        "NTFS"
    } else if buf.len() >= 512 && buf[510] == 0x55 && buf[511] == 0xAA {
        "tuntematon (0x55AA-allekirjoitus löytyy)"
    } else {
        "tuntematon"
    }
}

/// Kuinka monta FAT-merkintää tauluun mahtuu.
pub fn fat_entry_count(fat_len: usize, ft: FatType) -> u32 {
    match ft {
        FatType::Fat12 => (fat_len * 2 / 3) as u32,
        FatType::Fat16 => (fat_len / 2) as u32,
        FatType::Fat32 => (fat_len / 4) as u32,
    }
}

/// Ketjun loppu -merkintä (EOC).
pub fn eoc(ft: FatType) -> u32 {
    match ft {
        FatType::Fat12 => 0x0FF8,
        FatType::Fat16 => 0xFFF8,
        FatType::Fat32 => 0x0FFF_FFF8,
    }
}

/// Huono klusteri -merkintä (BAD).
pub fn bad(ft: FatType) -> u32 {
    match ft {
        FatType::Fat12 => 0x0FF7,
        FatType::Fat16 => 0xFFF7,
        FatType::Fat32 => 0x0FFF_FFF7,
    }
}

/// Lukee yhden FAT-merkinnän. Palauttaa 0, jos klusteri on taulun ulkopuolella.
pub fn fat_entry(fat: &[u8], cluster: u32, ft: FatType) -> u32 {
    match ft {
        FatType::Fat12 => {
            let o = (cluster as usize * 3) / 2;
            if o + 1 >= fat.len() {
                return 0;
            }
            let v = fat[o] as u32 | ((fat[o + 1] as u32) << 8);
            if cluster % 2 == 1 {
                v >> 4
            } else {
                v & 0x0FFF
            }
        }
        FatType::Fat16 => {
            let o = cluster as usize * 2;
            if o + 1 >= fat.len() {
                return 0;
            }
            u16::from_le_bytes([fat[o], fat[o + 1]]) as u32
        }
        FatType::Fat32 => {
            let o = cluster as usize * 4;
            if o + 3 >= fat.len() {
                return 0;
            }
            u32::from_le_bytes([fat[o], fat[o + 1], fat[o + 2], fat[o + 3]]) & 0x0FFF_FFFF
        }
    }
}

/// Muodostaa klusteriketjun FAT-taulusta. Katkaisee silmukat ja
/// epätavallisen pitkät ketjut.
pub fn cluster_chain(fat: &[u8], ft: FatType, start: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut c = start;
    let limit = 4_000_000;
    while c >= 2 && c < eoc(ft) && c != bad(ft) && seen.insert(c) && out.len() < limit {
        out.push(c);
        let next = fat_entry(fat, c, ft);
        if next < 2 {
            break;
        }
        c = next;
    }
    out
}

/// Lukee ensimmäisen FAT-taulun muistiin (enintään 64 MiB).
pub fn read_fat_table(dev: &mut Device, info: &FatInfo) -> io::Result<Option<Vec<u8>>> {
    let bytes = info.fat_size_sectors as u64 * info.bytes_per_sector as u64;
    if bytes == 0 || bytes > 64 * 1024 * 1024 {
        println!(
            "FAT-taulun koko ({} tavua) on nolla tai liian suuri - ohitetaan.",
            bytes
        );
        return Ok(None);
    }
    let fat = dev.read_at(
        info.fat_start() * info.bytes_per_sector as u64,
        bytes as usize,
    )?;
    Ok(Some(fat))
}

/// Tulostaa laitteen rakenteen ja palauttaa ensimmäisen löytyneen FAT-VBR:n tiedot.
pub fn analyze_device(dev: &mut Device) -> io::Result<Option<FatInfo>> {
    println!();
    println!("=== LAITTEEN RAKENNEANALYYSI ===");
    let s0 = dev.read_sectors(0, 1)?;
    if s0.is_empty() {
        println!("Laitetta ei voitu lukea (0 tavua).");
        return Ok(None);
    }
    let parts = parse_mbr(&s0);
    let mut found: Option<FatInfo> = None;
    // Volyymilaite (\\.\D:) alkaa suoraan FAT-boot-sektorista, joten se
    // kokeillaan ensin: MBR-jäsennys voi muuten tulkita VBR:n boot-koodin
    // vahingossa osiotauluksi.
    if let Some(info) = parse_fat_vbr(&s0, 0) {
        println!("Sektori 0 on FAT boot-sektori (volyymi tai superkiekko):");
        println!("  {}", info.describe());
        found = Some(info);
    } else if !parts.is_empty() {
        println!("Sektori 0 on MBR: siinä {} osiota:", parts.len());
        for p in &parts {
            let size_mib = p.sectors as f64 * 512.0 / (1024.0 * 1024.0);
            println!(
                "  #{} {} tyyppi: {} (0x{:02X}), LBA {}..{}, ~{:.1} MiB",
                p.index + 1,
                if p.bootable { "[käynnistys]" } else { "           " },
                partition_type_name(p.ptype),
                p.ptype,
                p.lba,
                p.lba + p.sectors.saturating_sub(1),
                size_mib
            );
            let vbr = dev.read_sectors(p.lba, 1)?;
            match parse_fat_vbr(&vbr, p.lba) {
                Some(info) => {
                    println!("    -> FAT boot-sektori OK:");
                    println!("       {}", info.describe());
                    if found.is_none() {
                        found = Some(info);
                    }
                }
                None => {
                    println!(
                        "    -> Ei FAT-boot-sektoria (tunnistus: {}).",
                        detect_fs(&vbr)
                    );
                }
            }
        }
    } else {
        println!("Sektori 0 ei sisällä tunnistettavaa FAT-rakennetta eikä MBR:ää.");
        println!("Tunnistus: {}", detect_fs(&s0));
    }
    if found.is_none() {
        println!();
        println!("FAT-rakennetta ei löytynyt - se ei haittaa signatuuriskannausta");
        println!("eikä sektoriselausta.");
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fat32_vbr() -> Vec<u8> {
        let mut b = vec![0u8; 512];
        b[11..13].copy_from_slice(&512u16.to_le_bytes());
        b[13] = 8; // sektoreita per klusteri
        b[14..16].copy_from_slice(&32u16.to_le_bytes()); // varatut
        b[16] = 2; // FAT-taulujen määrä
        b[32..36].copy_from_slice(&10_000_000u32.to_le_bytes()); // koko
        b[36..40].copy_from_slice(&9_710u32.to_le_bytes()); // FAT:n koko
        b[44..48].copy_from_slice(&2u32.to_le_bytes()); // juuriklusteri
        b[82..88].copy_from_slice(b"FAT32 ");
        b[510] = 0x55;
        b[511] = 0xAA;
        b
    }

    #[test]
    fn parses_fat32_vbr() {
        let info = parse_fat_vbr(&fat32_vbr(), 0).expect("pitäisi jäsentyä");
        assert_eq!(info.fat_type, FatType::Fat32);
        assert_eq!(info.bytes_per_sector, 512);
        assert_eq!(info.sectors_per_cluster, 8);
        assert_eq!(info.fat_size_sectors, 9_710);
        assert_eq!(info.root_cluster, 2);
        assert_eq!(info.data_start(), 32 + 2 * 9_710);
        assert_eq!(info.cluster_size(), 4096);
    }

    #[test]
    fn rejects_non_fat_sector() {
        let mut b = vec![0u8; 512];
        b[510] = 0x55;
        b[511] = 0xAA;
        assert!(parse_fat_vbr(&b, 0).is_none());
        assert_eq!(detect_fs(&b), "tuntematon (0x55AA-allekirjoitus löytyy)");
    }

    #[test]
    fn parses_mbr() {
        let mut b = vec![0u8; 512];
        b[446 + 4] = 0x0C; // FAT32 LBA
        b[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
        b[446 + 12..446 + 16].copy_from_slice(&1_000_000u32.to_le_bytes());
        b[510] = 0x55;
        b[511] = 0xAA;
        let parts = parse_mbr(&b);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].lba, 2048);
        assert_eq!(parts[0].sectors, 1_000_000);
        assert_eq!(partition_type_name(0x0C), "FAT32");
    }

    #[test]
    fn fat16_entries_and_chain() {
        // FAT16-taulu: klusteri 5 -> 6 -> 7 -> EOC
        let mut fat = vec![0u8; 64];
        fat[10..12].copy_from_slice(&6u16.to_le_bytes());
        fat[12..14].copy_from_slice(&7u16.to_le_bytes());
        fat[14..16].copy_from_slice(&0xFFF8u16.to_le_bytes());
        assert_eq!(fat_entry(&fat, 5, FatType::Fat16), 6);
        assert_eq!(fat_entry(&fat, 6, FatType::Fat16), 7);
        assert_eq!(fat_entry(&fat, 7, FatType::Fat16), 0xFFF8);
        let chain = cluster_chain(&fat, FatType::Fat16, 5);
        assert_eq!(chain, vec![5, 6, 7]);
    }

    #[test]
    fn fat12_entries() {
        let mut fat = vec![0u8; 16];
        // klusteri 3 (pariton): v = fat[4] | fat[5]<<8, arvo = v >> 4
        fat[4] = 0x34;
        fat[5] = 0x12; // v = 0x1234 -> 0x123
        // klusteri 4 (parillinen): v = fat[6] | fat[7]<<8, arvo = v & 0xFFF
        fat[6] = 0xBC;
        fat[7] = 0x0A; // v = 0x0ABC -> 0xABC
        assert_eq!(fat_entry(&fat, 3, FatType::Fat12), 0x123);
        assert_eq!(fat_entry(&fat, 4, FatType::Fat12), 0xABC);
        assert_eq!(eoc(FatType::Fat12), 0x0FF8);
        assert_eq!(bad(FatType::Fat12), 0x0FF7);
    }

    #[test]
    fn fat32_entry_is_masked() {
        let mut fat = vec![0u8; 16];
        // Ylin nibble (0xF) ei kuulu FAT32-merkintään ja maskataan pois.
        fat[8..12].copy_from_slice(&0xF000_0002u32.to_le_bytes());
        assert_eq!(fat_entry(&fat, 2, FatType::Fat32), 2);
        assert_eq!(fat_entry_count(16, FatType::Fat32), 4);
    }
}