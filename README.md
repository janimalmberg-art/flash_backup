# Muistikortin syväpalautus 🗂️

Suomenkielinen komentorivityökalu, joka palauttaa poistettuja tiedostoja
muistikorteilta ja muilta FAT-tiedostojärjestelmän levyiltä **raakatavuja
lukemalla** — toimii myös silloin, kun kortti on formatoitu tai
tiedostojärjestelmä on muuten vaurioitunut.

Ohjelma avaa lähteen **vain lukutilassa**: kortille ei kirjoiteta mitään.

## Ominaisuudet

- **Syväpalautus** — kävelee koko hakemistopuun (myös alihakemistot) ja
  löytää sekä elävät että **poistetut** tiedostot (0xE5-merkinnät)
- **Pitkien nimien palautus (LFN)** — poistettujenkin tiedostojen
  alkuperäiset pitkät nimet rakennetaan uudelleen; 8.3-nimen ensimmäinen
  kirjain päätellään LFN-tarkistussumman perusteella
- **Signatuuriskannaus (carving)** — 7 tiedostomuotoa (WAV, PNG, JPEG,
  ZIP, MP4, BMP, SQLite) otsikkotarkistuksilla vääräosumien suodatukseen
- **Levykuvatuki** — käsittelee myös `.img` / `.dd` / `.raw` -tiedostot,
  ei vain fyysisiä kortteja
- **FAT12/16/32** täysi tuki, myös juurihakemisto- ja alihakemistoketjut
- **Sektoriselain** — hexdump, merkkijonohaku ja alueiden tallennus
- **19 automatisoitua testiä**, mukaan lukien loppuun asti menevät
  kokonaisuustestit, jotka rakentavat FAT32/FAT16-kortit, poistavat
  tiedostoja Windowsin tapaan ja varmistavat täsmälleen saman sisällön

## Käyttö

> ⚠️ **Vaatii järjestelmänvalvojan oikeudet** — kortin raakaluku ei onnistu
> ilman niitä. **Älä kirjoita kortille mitään ennen palautusta.**

### Pakettitila (yksi komento, ei valikoita)

```bat
muistikortti_palautus.exe D
muistikortti_palautus.exe PHYSICALDRIVE1
muistikortti_palautus.exe C:\polku\kortti.img
```

Lähde voi olla asematunnus (`D`), fyysinen levy (`PHYSICALDRIVE1`) tai
levykuvatiedosto. Työkalu analysoi lähteen, palauttaa löydetyt poistetut
tiedostot ja ajaa signatuuriskannauksen.

### Vuorovaikutteinen tila

Ilman argumentteja käynnistyy valikko-ohjelma: analyysi, perinteinen
palautus, syväpalautus, signatuuriskannaus ja sektoriselain.

Windows-käyttäjille mukana `KAYNNISTA.bat`, joka kohottaa oikeudet
automaattisesti.

### Tulokset

| Kansio | Sisältö |
|---|---|
| `palautetut\` | syväpalautuksen ja selauksen palauttamat tiedostot |
| `palautetut_carve\` | signatuuriskannauksen palauttamat tiedostot |

## Kääntäminen ja testit

```bat
cargo build --release
cargo test
```

Vaaditaan [Rust](https://rustup.rs). Testit rakentavat väliaikaisia
FAT-korttikuvia `%TEMP%\muistikortti_palautus_testit`-kansioon.

## Lähdekoodin rakenne

| Tiedosto | Tehtävä |
|---|---|
| `src\main.rs` | valikot ja pakettitila |
| `src\rawdev.rs` | raakalaitteen ja levykuvan avaus |
| `src\fatinfo.rs` | FAT-analyysi (MBR, boot-sektori, FAT-taulu) |
| `src\fatwalk.rs` | hakemistopuu, LFN-nimet, poistettujen palautus |
| `src\carve.rs` | signatuuriskannaus |
| `src\browser.rs` | sektoriselain |
| `src\selftest.rs` | kokonaisuustestit |

## Rajoitukset ja eettiset ohjeet

- exFAT- ja NTFS-korteissa toimii vain signatuuriskannaus
- Pirstaleiset poistetut tiedostot voivat palautua epätäydellisinä —
  työkalu ilmoittaa, kun data luettiin peräkkäin arvaamalla
- Ylikirjoitettua dataa ei mikään työkalu voi palauttaa
- Palauta **vain omia korttejasi tai dataa, johon sinulla on lupa**.
  Työkalu on tarkoitettu oman datan pelastamiseen ja opetuskäyttöön.

## Julkaisut

GitHub Actions rakentaa ja testaa jokaisen muutoksen automaattisesti.
Kun pushaat tunnisteen `v0.2.0`, työnkuo `release.yml` rakentaa
Windows-exen ja liittää zip-paketin suoraan GitHub Releases -sivulle:

```bat
git tag v0.2.0
git push origin v0.2.0
```

## Lisenssi

[MIT](LICENSE) — tee mitä haluat, älä takaa mitään.