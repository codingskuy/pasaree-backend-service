//! Cek otomatis produk (SOP Tayang §3, BR-201..BR-206). Murni, tanpa DB, sehingga mudah diuji.

use chrono::NaiveDate;

use crate::error::KesalahanKolom;

pub const FOTO_MIN_PX: i32 = 800;
pub const FOTO_MAKS_BYTE: i64 = 5 * 1024 * 1024;
pub const VIDEO_MAKS_BYTE: i64 = 50 * 1024 * 1024;
pub const AMBANG_KEMIRIPAN: f64 = 0.85;

pub struct KategoriInfo {
    pub butuh_izin_pangan: bool,
    pub butuh_kedaluwarsa: bool,
}

pub struct VarianData {
    pub nama_varian: String,
    pub sku: String,
    pub harga_idr: i32,
    pub stok: i32,
    pub berat_kirim_gram: i32,
    pub tanggal_produksi: Option<NaiveDate>,
    pub kedaluwarsa: Option<NaiveDate>,
}

pub struct MediaData {
    pub jenis: String,
    pub utama: bool,
    pub lebar: Option<i32>,
    pub tinggi: Option<i32>,
    pub ukuran_byte: Option<i64>,
}

pub struct ProdukData {
    pub judul: String,
    pub tag: Vec<String>,
    pub deskripsi: String,
    pub komposisi: Option<String>,
    pub alergen: Option<Vec<String>>,
    pub varian: Vec<VarianData>,
    pub media: Vec<MediaData>,
}

fn normalisasi(s: &str) -> Vec<char> {
    let mut hasil = Vec::new();
    let mut spasi = true;
    for c in s.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_alphanumeric() {
            hasil.push(c);
            spasi = false;
        } else if !spasi {
            hasil.push(' ');
            spasi = true;
        }
    }
    while hasil.last() == Some(&' ') {
        hasil.pop();
    }
    hasil
}

/// Kemiripan judul 0..1 = 1 - jarak Levenshtein / panjang terpanjang (setelah normalisasi).
pub fn kemiripan(a: &str, b: &str) -> f64 {
    let (a, b) = (normalisasi(a), normalisasi(b));
    let maks = a.len().max(b.len());
    if maks == 0 {
        return 1.0;
    }
    let mut sebelum: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut kini = vec![i];
        for j in 1..=b.len() {
            let biaya = usize::from(a[i - 1] != b[j - 1]);
            kini.push(
                (sebelum[j] + 1)
                    .min(kini[j - 1] + 1)
                    .min(sebelum[j - 1] + biaya),
            );
        }
        sebelum = kini;
    }
    1.0 - sebelum[b.len()] as f64 / maks as f64
}

/// Judul mirip (>= 85%) dengan salah satu judul lain.
pub fn judul_duplikat(judul: &str, lain: &[String]) -> Option<String> {
    lain.iter()
        .find(|j| kemiripan(judul, j) >= AMBANG_KEMIRIPAN)
        .cloned()
}

const SATUAN_JUDUL: &[&str] = &[
    "g", "gr", "gram", "kg", "ons", "ml", "l", "liter", "pcs", "pc", "pak", "paket", "isi",
    "butir", "buah", "botol", "sachet", "porsi", "bungkus", "toples", "lembar", "kaleng",
    "stoples", "box",
];

/// Judul wajib memuat berat/isi, mis. "150 g", "250gr", "isi 6".
pub fn judul_memuat_berat(judul: &str) -> bool {
    let kecil: Vec<char> = judul.to_lowercase().chars().collect();
    let n = kecil.len();
    let baca_huruf = |mut k: usize| {
        let mulai = k;
        while k < n && kecil[k].is_alphabetic() {
            k += 1;
        }
        kecil[mulai..k].iter().collect::<String>()
    };
    let mut i = 0;
    while i < n {
        if kecil[i].is_ascii_digit() {
            // "isi 6" / "isi6": satuan sebelum angka
            let sebelum: String = {
                let mut k = i;
                while k > 0 && kecil[k - 1] == ' ' {
                    k -= 1;
                }
                let akhir = k;
                while k > 0 && kecil[k - 1].is_alphabetic() {
                    k -= 1;
                }
                kecil[k..akhir].iter().collect()
            };
            if sebelum == "isi" {
                return true;
            }
            while i < n && (kecil[i].is_ascii_digit() || kecil[i] == '.' || kecil[i] == ',') {
                i += 1;
            }
            while i < n && kecil[i] == ' ' {
                i += 1;
            }
            if SATUAN_JUDUL.contains(&baca_huruf(i).as_str()) {
                return true;
            }
        } else {
            i += 1;
        }
    }
    false
}

pub fn harga_valid(h: i32) -> bool {
    h > 0 && h % 500 == 0
}

pub fn cek_otomatis(
    p: &ProdukData,
    kat: &KategoriInfo,
    judul_lain_lapak: &[String],
) -> Vec<KesalahanKolom> {
    let mut e = Vec::new();
    let panjang_judul = p.judul.chars().count();
    if !(10..=80).contains(&panjang_judul) {
        e.push(KesalahanKolom::baru("judul", "Judul harus 10-80 karakter."));
    }
    if !judul_memuat_berat(&p.judul) {
        e.push(KesalahanKolom::baru(
            "judul",
            "Judul wajib memuat berat atau isi (contoh: 150 g).",
        ));
    }
    if let Some(mirip) = judul_duplikat(&p.judul, judul_lain_lapak) {
        e.push(KesalahanKolom::baru(
            "judul",
            format!("Judul terlalu mirip (>= 85%) dengan produk lain di lapak: \"{mirip}\"."),
        ));
    }
    if !(50..=2000).contains(&p.deskripsi.chars().count()) {
        e.push(KesalahanKolom::baru(
            "deskripsi",
            "Deskripsi harus 50-2000 karakter.",
        ));
    }
    if p.tag.len() > 2 {
        e.push(KesalahanKolom::baru("tag", "Tag maksimal 2."));
    }
    if kat.butuh_izin_pangan {
        if p.komposisi
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            e.push(KesalahanKolom::baru(
                "komposisi",
                "Komposisi wajib untuk produk pangan.",
            ));
        }
        if p.alergen.is_none() {
            e.push(KesalahanKolom::baru(
                "alergen",
                "Alergen wajib diisi untuk produk pangan (boleh kosong bila tanpa alergen).",
            ));
        }
    }
    if p.varian.is_empty() {
        e.push(KesalahanKolom::baru("varian", "Minimal satu varian."));
    }
    for (i, v) in p.varian.iter().enumerate() {
        let k = |f: &str| format!("varian[{i}].{f}");
        if !harga_valid(v.harga_idr) {
            e.push(KesalahanKolom::baru(
                k("harga_idr"),
                "Harga harus lebih dari 0 dan kelipatan Rp500.",
            ));
        }
        if v.stok < 0 {
            e.push(KesalahanKolom::baru(k("stok"), "Stok tidak boleh negatif."));
        }
        if v.berat_kirim_gram <= 0 {
            e.push(KesalahanKolom::baru(
                k("berat_kirim_gram"),
                "Berat kirim harus lebih dari 0.",
            ));
        }
        if kat.butuh_kedaluwarsa {
            if v.tanggal_produksi.is_none() {
                e.push(KesalahanKolom::baru(
                    k("tanggal_produksi"),
                    "Tanggal produksi wajib untuk produk pangan.",
                ));
            }
            match (v.tanggal_produksi, v.kedaluwarsa) {
                (_, None) => e.push(KesalahanKolom::baru(
                    k("kedaluwarsa"),
                    "Tanggal kedaluwarsa wajib untuk produk pangan.",
                )),
                (Some(prod), Some(kdl)) if kdl <= prod => e.push(KesalahanKolom::baru(
                    k("kedaluwarsa"),
                    "Kedaluwarsa harus setelah tanggal produksi.",
                )),
                _ => {}
            }
        }
    }
    let foto: Vec<&MediaData> = p.media.iter().filter(|m| m.jenis == "FOTO").collect();
    if foto.is_empty() {
        e.push(KesalahanKolom::baru("media", "Minimal satu foto."));
    }
    if p.media.iter().filter(|m| m.utama).count() != 1 {
        e.push(KesalahanKolom::baru("media", "Tepat satu media utama."));
    }
    if let Some(u) = p.media.iter().find(|m| m.utama) {
        if u.jenis != "FOTO" {
            e.push(KesalahanKolom::baru("media", "Media utama harus foto."));
        }
    }
    for (i, m) in p.media.iter().enumerate() {
        let k = format!("media[{i}]");
        if m.jenis == "FOTO" {
            match (m.lebar, m.tinggi) {
                (Some(l), Some(t)) if l >= FOTO_MIN_PX && t >= FOTO_MIN_PX => {}
                (Some(_), Some(_)) => {
                    e.push(KesalahanKolom::baru(k.clone(), "Foto minimal 800x800 px."))
                }
                _ => e.push(KesalahanKolom::baru(
                    k.clone(),
                    "Dimensi foto (lebar, tinggi) wajib diisi.",
                )),
            }
            if m.ukuran_byte.is_some_and(|b| b > FOTO_MAKS_BYTE) {
                e.push(KesalahanKolom::baru(k, "Foto maksimal 5 MB."));
            }
        } else if m.ukuran_byte.is_some_and(|b| b > VIDEO_MAKS_BYTE) {
            e.push(KesalahanKolom::baru(k, "Video maksimal 50 MB."));
        }
    }
    e
}

/// Tambah `n` hari kerja (Sen-Jum) pada waktu `dari`; dipakai untuk estimasi SLA 2x24 jam kerja.
pub fn tambah_hari_kerja(
    dari: chrono::DateTime<chrono::Utc>,
    n: u32,
) -> chrono::DateTime<chrono::Utc> {
    use chrono::{Datelike, Duration, Weekday};
    let mut t = dari;
    let mut sisa = n;
    while sisa > 0 {
        t += Duration::days(1);
        if !matches!(t.weekday(), Weekday::Sat | Weekday::Sun) {
            sisa -= 1;
        }
    }
    t
}

#[cfg(test)]
mod uji {
    use super::*;

    fn kat(pangan: bool) -> KategoriInfo {
        KategoriInfo {
            butuh_izin_pangan: pangan,
            butuh_kedaluwarsa: pangan,
        }
    }

    fn produk_baik() -> ProdukData {
        ProdukData {
            judul: "Sambal Bawang Mbok D — 150 g".into(),
            tag: vec!["pedas".into()],
            deskripsi: "Sambal bawang buatan rumahan, pedas gurih, tanpa pengawet, dikemas higienis dalam botol kaca.".into(),
            komposisi: Some("Bawang, cabai, garam".into()),
            alergen: Some(vec![]),
            varian: vec![VarianData {
                nama_varian: "Original".into(),
                sku: "LPK-ABCD-0001".into(),
                harga_idr: 18000,
                stok: 10,
                berat_kirim_gram: 200,
                tanggal_produksi: NaiveDate::from_ymd_opt(2026, 10, 1),
                kedaluwarsa: NaiveDate::from_ymd_opt(2027, 1, 1),
            }],
            media: vec![MediaData { jenis: "FOTO".into(), utama: true, lebar: Some(1200), tinggi: Some(1200), ukuran_byte: Some(900_000) }],
        }
    }

    #[test]
    fn produk_lengkap_lolos() {
        assert!(cek_otomatis(&produk_baik(), &kat(true), &[]).is_empty());
    }

    #[test]
    fn judul_batas_panjang_dan_berat() {
        let mut p = produk_baik();
        p.judul = "Sambal 1 g".into(); // 10 karakter, ada berat
        assert!(cek_otomatis(&p, &kat(true), &[]).is_empty());
        p.judul = "Sambal 5 g".into();
        p.judul.pop();
        let e = cek_otomatis(&p, &kat(true), &[]);
        assert!(e.iter().any(|x| x.kolom == "judul"));
        p.judul = "x".repeat(81);
        assert!(cek_otomatis(&p, &kat(true), &[])
            .iter()
            .any(|x| x.pesan.contains("10-80")));
        p.judul = "Sambal Bawang Enak Sekali".into();
        assert!(cek_otomatis(&p, &kat(true), &[])
            .iter()
            .any(|x| x.pesan.contains("berat")));
    }

    #[test]
    fn deteksi_berat_isi() {
        for ok in [
            "Sambal 150 g",
            "Kopi Arabika 250gr",
            "Madu 1,5 kg",
            "Keripik isi 6",
            "Susu 1 l",
            "Teh 20 sachet",
        ] {
            assert!(judul_memuat_berat(ok), "{ok}");
        }
        for salah in ["Sambal Bawang Mbok D", "Produk nomor 5 enak", "Kue 2 hari"] {
            assert!(!judul_memuat_berat(salah), "{salah}");
        }
    }

    #[test]
    fn kemiripan_judul_85_persen_ditolak() {
        assert!(kemiripan("Sambal Bawang 150 g", "sambal  bawang 150 G") >= 0.99);
        // 1 karakter beda dari 20 -> 95%
        assert!(judul_duplikat(
            "Sambal Bawang Mbok 150 g",
            &["Sambal Bawang Mbok 150 gr".into()]
        )
        .is_some());
        // jelas berbeda
        assert!(judul_duplikat(
            "Sambal Bawang Mbok 150 g",
            &["Keripik Singkong Pedas 200 g".into()]
        )
        .is_none());
        // tepat di bawah ambang: 7 dari 20 karakter beda -> 65%
        assert!(kemiripan("abcdefghij", "abcdefgxyz") < AMBANG_KEMIRIPAN);
        // tepat ambang 85% (3 beda dari 20) masih ditolak
        assert!(kemiripan("aaaaaaaaaaaaaaaaaaaa", "aaaaaaaaaaaaaaaaaxxx") >= AMBANG_KEMIRIPAN);
        let e = cek_otomatis(
            &produk_baik(),
            &kat(true),
            &["Sambal Bawang Mbok D — 150 gr".into()],
        );
        assert!(e.iter().any(|x| x.pesan.contains("terlalu mirip")));
    }

    #[test]
    fn harga_kelipatan_500_dan_positif() {
        assert!(harga_valid(500) && harga_valid(18000));
        assert!(!harga_valid(0) && !harga_valid(-500) && !harga_valid(18200) && !harga_valid(250));
        let mut p = produk_baik();
        p.varian[0].harga_idr = 18200;
        assert!(cek_otomatis(&p, &kat(true), &[])
            .iter()
            .any(|x| x.kolom == "varian[0].harga_idr"));
    }

    #[test]
    fn foto_resolusi_dan_ukuran() {
        let mut p = produk_baik();
        p.media[0].lebar = Some(799);
        assert!(cek_otomatis(&p, &kat(true), &[])
            .iter()
            .any(|x| x.pesan.contains("800x800")));
        p.media[0].lebar = Some(800);
        p.media[0].tinggi = Some(800);
        assert!(cek_otomatis(&p, &kat(true), &[]).is_empty());
        p.media[0].ukuran_byte = Some(FOTO_MAKS_BYTE + 1);
        assert!(cek_otomatis(&p, &kat(true), &[])
            .iter()
            .any(|x| x.pesan.contains("5 MB")));
        p.media[0].ukuran_byte = Some(FOTO_MAKS_BYTE);
        assert!(cek_otomatis(&p, &kat(true), &[]).is_empty());
        p.media.clear();
        assert!(cek_otomatis(&p, &kat(true), &[])
            .iter()
            .any(|x| x.pesan.contains("satu foto")));
    }

    #[test]
    fn kolom_pangan_wajib() {
        let mut p = produk_baik();
        p.komposisi = None;
        p.alergen = None;
        p.varian[0].tanggal_produksi = None;
        p.varian[0].kedaluwarsa = None;
        let e = cek_otomatis(&p, &kat(true), &[]);
        for kolom in [
            "komposisi",
            "alergen",
            "varian[0].tanggal_produksi",
            "varian[0].kedaluwarsa",
        ] {
            assert!(e.iter().any(|x| x.kolom == kolom), "{kolom}");
        }
        // non-pangan tidak diwajibkan
        assert!(cek_otomatis(&p, &kat(false), &[]).is_empty());
    }

    #[test]
    fn hari_kerja_melewati_akhir_pekan() {
        use chrono::TimeZone;
        // Kamis 2026-10-22 + 2 hari kerja = Senin 2026-10-26
        let kamis = chrono::Utc.with_ymd_and_hms(2026, 10, 22, 9, 0, 0).unwrap();
        assert_eq!(
            tambah_hari_kerja(kamis, 2).format("%Y-%m-%d").to_string(),
            "2026-10-26"
        );
        let senin = chrono::Utc.with_ymd_and_hms(2026, 10, 19, 9, 0, 0).unwrap();
        assert_eq!(
            tambah_hari_kerja(senin, 2).format("%Y-%m-%d").to_string(),
            "2026-10-21"
        );
    }
}
