//! Tipe bersama katalog: enum baku, DTO keluaran, dan muat data produk.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{
    cek::{KategoriInfo, MediaData, ProdukData, VarianData},
    error::{AppError, AppResult},
    ids,
};

macro_rules! enum_str {
    ($(#[$m:meta])* $name:ident { $($var:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $s)] $var),+ }
        impl $name {
            pub fn sebagai_str(self) -> &'static str {
                match self { $(Self::$var => $s),+ }
            }
            pub fn dari_str(s: &str) -> Option<Self> {
                match s { $($s => Some(Self::$var),)+ _ => None }
            }
        }
    };
}

enum_str!(KategoriKode {
    PanganSegar => "pangan-segar",
    PanganOlahan => "pangan-olahan",
    BumbuDapur => "bumbu-dapur",
    Minuman => "minuman",
    KerajinanPangan => "kerajinan-pangan",
    PaketHampers => "paket-hampers",
});

enum_str!(Tag {
    Pedas => "pedas",
    Manis => "manis",
    Vegan => "vegan",
    GlutenFree => "gluten-free",
    Frozen => "frozen",
    SiapSaji => "siap-saji",
    KemasanUlang => "kemasan-ulang",
    EdisiMusiman => "edisi-musiman",
});

enum_str!(Satuan {
    G => "g",
    Ml => "ml",
    Pcs => "pcs",
    Paket => "paket",
});

enum_str!(JenisMedia {
    Foto => "FOTO",
    Video => "VIDEO",
});

enum_str!(JenisProdusen {
    Perorangan => "PERORANGAN",
    Umkm => "UMKM",
    Koperasi => "KOPERASI",
    DapurBersama => "DAPUR_BERSAMA",
});

enum_str!(ProdukStatus {
    Draf => "DRAF",
    MenungguKurasi => "MENUNGGU_KURASI",
    PerluRevisi => "PERLU_REVISI",
    Tayang => "TAYANG",
    NonaktifStok0 => "NONAKTIF_STOK_0",
    Takedown => "TAKEDOWN",
    Arsip => "ARSIP",
});

#[derive(Serialize, sqlx::FromRow)]
pub struct VarianRow {
    #[serde(serialize_with = "ser_uuid")]
    pub id: Uuid,
    #[serde(serialize_with = "ser_uuid")]
    pub produk_id: Uuid,
    pub nama_varian: String,
    pub sku: String,
    pub harga_idr: i32,
    pub stok: i32,
    pub satuan: String,
    pub berat_kirim_gram: i32,
    pub tanggal_produksi: Option<NaiveDate>,
    pub kedaluwarsa: Option<NaiveDate>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct MediaRow {
    pub url: String,
    pub jenis: String,
    pub utama: bool,
    pub lebar: Option<i32>,
    pub tinggi: Option<i32>,
    pub ukuran_byte: Option<i64>,
}

pub fn ser_uuid<S: serde::Serializer>(id: &Uuid, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&ids::ke_ulid(*id))
}

pub async fn muat_varian(c: &mut PgConnection, produk_id: Uuid) -> AppResult<Vec<VarianRow>> {
    Ok(sqlx::query_as(
        "SELECT id, produk_id, nama_varian, sku, harga_idr, stok, satuan, berat_kirim_gram, tanggal_produksi, kedaluwarsa \
         FROM varian_produk WHERE produk_id = $1 ORDER BY sku, id",
    )
    .bind(produk_id)
    .fetch_all(c)
    .await?)
}

pub async fn muat_media(c: &mut PgConnection, produk_id: Uuid) -> AppResult<Vec<MediaRow>> {
    Ok(sqlx::query_as(
        "SELECT url, jenis, utama, lebar, tinggi, ukuran_byte FROM media_produk WHERE produk_id = $1 ORDER BY urutan, id",
    )
    .bind(produk_id)
    .fetch_all(c)
    .await?)
}

#[derive(sqlx::FromRow)]
pub struct ProdukRow {
    pub id: Uuid,
    pub lapak_id: Uuid,
    pub judul: String,
    pub kategori_kode: String,
    pub tag: Vec<String>,
    pub deskripsi: String,
    pub komposisi: Option<String>,
    pub alergen: Option<Vec<String>>,
    pub status: String,
    pub perlu_revisi_catatan: Option<String>,
}

pub async fn muat_produk(c: &mut PgConnection, id: Uuid) -> AppResult<ProdukRow> {
    sqlx::query_as(
        "SELECT id, lapak_id, judul, kategori_kode, tag, deskripsi, komposisi, alergen, status, perlu_revisi_catatan \
         FROM produk WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(c)
    .await?
    .ok_or_else(|| AppError::tidak_ditemukan("PRODUK_TIDAK_DITEMUKAN", "Produk tidak ditemukan."))
}

pub async fn muat_kategori(c: &mut PgConnection, kode: &str) -> AppResult<KategoriInfo> {
    let (izin, kdl): (bool, bool) =
        sqlx::query_as("SELECT butuh_izin_pangan, butuh_kedaluwarsa FROM kategori WHERE kode = $1")
            .bind(kode)
            .fetch_one(c)
            .await?;
    Ok(KategoriInfo {
        butuh_izin_pangan: izin,
        butuh_kedaluwarsa: kdl,
    })
}

/// Judul produk lain di lapak yang sama (selain ARSIP/TAKEDOWN) untuk cek kemiripan.
pub async fn judul_lain(
    c: &mut PgConnection,
    lapak_id: Uuid,
    kecuali: Option<Uuid>,
) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT judul FROM produk WHERE lapak_id = $1 AND status NOT IN ('ARSIP','TAKEDOWN') AND ($2::uuid IS NULL OR id <> $2)",
    )
    .bind(lapak_id)
    .bind(kecuali)
    .fetch_all(c)
    .await?)
}

/// Susun data untuk cek otomatis dari DB.
pub async fn data_cek(
    c: &mut PgConnection,
    p: &ProdukRow,
) -> AppResult<(ProdukData, KategoriInfo)> {
    let kat = muat_kategori(c, &p.kategori_kode).await?;
    let varian = muat_varian(c, p.id)
        .await?
        .into_iter()
        .map(|v| VarianData {
            nama_varian: v.nama_varian,
            sku: v.sku,
            harga_idr: v.harga_idr,
            stok: v.stok,
            berat_kirim_gram: v.berat_kirim_gram,
            tanggal_produksi: v.tanggal_produksi,
            kedaluwarsa: v.kedaluwarsa,
        })
        .collect();
    let media = muat_media(c, p.id)
        .await?
        .into_iter()
        .map(|m| MediaData {
            jenis: m.jenis,
            utama: m.utama,
            lebar: m.lebar,
            tinggi: m.tinggi,
            ukuran_byte: m.ukuran_byte,
        })
        .collect();
    Ok((
        ProdukData {
            judul: p.judul.clone(),
            tag: p.tag.clone(),
            deskripsi: p.deskripsi.clone(),
            komposisi: p.komposisi.clone(),
            alergen: p.alergen.clone(),
            varian,
            media,
        },
        kat,
    ))
}

/// Sinkronkan TAYANG <-> NONAKTIF_STOK_0 menurut total stok varian. Kembalikan status terbaru.
pub async fn sinkron_status_stok(c: &mut PgConnection, produk_id: Uuid) -> AppResult<String> {
    Ok(sqlx::query_scalar(
        "UPDATE produk p SET status = CASE \
            WHEN p.status = 'TAYANG' AND s.total = 0 THEN 'NONAKTIF_STOK_0' \
            WHEN p.status = 'NONAKTIF_STOK_0' AND s.total > 0 THEN 'TAYANG' \
            ELSE p.status END, diperbarui_pada = now() \
         FROM (SELECT COALESCE(SUM(stok), 0) AS total FROM varian_produk WHERE produk_id = $1) s \
         WHERE p.id = $1 RETURNING p.status",
    )
    .bind(produk_id)
    .fetch_one(c)
    .await?)
}
