-- PSR-BE-004: produsen, lapak, kategori, produk, varian, media, riwayat harga, kurasi.
-- ID = uuid (bit sama dengan ULID publik). Uang integer IDR.

CREATE TABLE kategori (
    kode text PRIMARY KEY,
    nama text NOT NULL,
    butuh_izin_pangan boolean NOT NULL,
    butuh_kedaluwarsa boolean NOT NULL
);
INSERT INTO kategori (kode, nama, butuh_izin_pangan, butuh_kedaluwarsa) VALUES
    ('pangan-segar', 'Pangan Segar', true, true),
    ('pangan-olahan', 'Pangan Olahan', true, true),
    ('bumbu-dapur', 'Bumbu Dapur', false, false),
    ('minuman', 'Minuman', true, true),
    ('kerajinan-pangan', 'Kerajinan Pangan', false, false),
    ('paket-hampers', 'Paket Hampers', false, false);

CREATE TABLE produsen (
    id uuid PRIMARY KEY,
    pengguna_id uuid NOT NULL UNIQUE REFERENCES pengguna (id),
    nama text NOT NULL CHECK (char_length(nama) BETWEEN 3 AND 80),
    jenis text NOT NULL CHECK (jenis IN ('PERORANGAN', 'UMKM', 'KOPERASI', 'DAPUR_BERSAMA')),
    identitas_terverifikasi boolean NOT NULL DEFAULT false,
    kontak_wa text NOT NULL,
    dapur_bersama_id text,
    dibuat_pada timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE lapak (
    id uuid PRIMARY KEY,
    produsen_id uuid NOT NULL UNIQUE REFERENCES produsen (id),
    nama_lapak text NOT NULL CHECK (char_length(nama_lapak) BETWEEN 3 AND 60),
    slug text NOT NULL UNIQUE CHECK (slug ~ '^[a-z0-9-]{3,60}$'),
    deskripsi text NOT NULL CHECK (char_length(deskripsi) BETWEEN 50 AND 500),
    status text NOT NULL DEFAULT 'AKTIF' CHECK (status IN ('AKTIF', 'BEKU', 'TUTUP')),
    titik_serah_lumbung_id text NOT NULL,
    -- hanya 4 digit terakhir; nomor lengkap dikelola skema Lumbung.
    rekening_payout_tersensor text NOT NULL,
    produk_pertama_tayang_pada timestamptz,
    dibuat_pada timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX lapak_nama_unik ON lapak (lower(nama_lapak));

CREATE TABLE lapak_operator (
    lapak_id uuid NOT NULL REFERENCES lapak (id) ON DELETE CASCADE,
    pengguna_id uuid NOT NULL REFERENCES pengguna (id) ON DELETE CASCADE,
    dibuat_pada timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (lapak_id, pengguna_id)
);

CREATE TABLE produk (
    id uuid PRIMARY KEY,
    lapak_id uuid NOT NULL REFERENCES lapak (id),
    judul text NOT NULL,
    kategori_kode text NOT NULL REFERENCES kategori (kode),
    tag text[] NOT NULL DEFAULT '{}' CHECK (cardinality(tag) <= 2),
    deskripsi text NOT NULL,
    komposisi text,
    alergen text[],
    status text NOT NULL DEFAULT 'DRAF' CHECK (status IN
        ('DRAF', 'MENUNGGU_KURASI', 'PERLU_REVISI', 'TAYANG', 'NONAKTIF_STOK_0', 'TAKEDOWN', 'ARSIP')),
    rating_rata double precision NOT NULL DEFAULT 0 CHECK (rating_rata BETWEEN 0 AND 5),
    total_ulasan integer NOT NULL DEFAULT 0,
    perlu_revisi_catatan text,
    perlu_revisi_pada timestamptz,
    diajukan_pada timestamptz,
    dibuat_pada timestamptz NOT NULL DEFAULT now(),
    diperbarui_pada timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX produk_lapak ON produk (lapak_id, status);
CREATE INDEX produk_status ON produk (status);

CREATE TABLE varian_produk (
    id uuid PRIMARY KEY,
    produk_id uuid NOT NULL REFERENCES produk (id),
    nama_varian text NOT NULL CHECK (char_length(nama_varian) BETWEEN 2 AND 40),
    sku text NOT NULL UNIQUE CHECK (sku ~ '^LPK-[A-Z0-9]{4}-[0-9]{4}$'),
    harga_idr integer NOT NULL CHECK (harga_idr > 0 AND harga_idr % 500 = 0),
    stok integer NOT NULL CHECK (stok >= 0),
    satuan text NOT NULL CHECK (satuan IN ('g', 'ml', 'pcs', 'paket')),
    berat_kirim_gram integer NOT NULL CHECK (berat_kirim_gram > 0),
    tanggal_produksi date,
    kedaluwarsa date,
    diperbarui_pada timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX varian_produk_produk ON varian_produk (produk_id);

CREATE TABLE media_produk (
    id uuid PRIMARY KEY,
    produk_id uuid NOT NULL REFERENCES produk (id),
    url text NOT NULL CHECK (url ~ '^https://'),
    jenis text NOT NULL CHECK (jenis IN ('FOTO', 'VIDEO')),
    utama boolean NOT NULL DEFAULT false,
    lebar integer,
    tinggi integer,
    ukuran_byte bigint,
    urutan integer NOT NULL DEFAULT 0
);
CREATE INDEX media_produk_produk ON media_produk (produk_id);
CREATE UNIQUE INDEX media_produk_satu_utama ON media_produk (produk_id) WHERE utama;

-- Riwayat harga: append-only (UPDATE/DELETE ditolak oleh trigger).
CREATE TABLE harga_riwayat (
    id uuid PRIMARY KEY,
    produk_id uuid NOT NULL REFERENCES produk (id),
    varian_id uuid NOT NULL REFERENCES varian_produk (id),
    harga_lama integer,
    harga_baru integer NOT NULL CHECK (harga_baru > 0),
    diubah_oleh uuid NOT NULL REFERENCES pengguna (id),
    diubah_pada timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX harga_riwayat_varian ON harga_riwayat (varian_id, diubah_pada);

CREATE FUNCTION tolak_ubah_append_only() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'tabel % append-only', TG_TABLE_NAME USING ERRCODE = '23000';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER harga_riwayat_append_only
    BEFORE UPDATE OR DELETE ON harga_riwayat
    FOR EACH ROW EXECUTE FUNCTION tolak_ubah_append_only();

-- Audit keputusan kurasi: siapa, kapan, alasan, snapshot konten.
CREATE TABLE kurasi_keputusan (
    id uuid PRIMARY KEY,
    produk_id uuid NOT NULL REFERENCES produk (id),
    kurator_id uuid NOT NULL REFERENCES pengguna (id),
    keputusan text NOT NULL CHECK (keputusan IN ('SETUJU', 'REVISI', 'TOLAK')),
    alasan_baku text,
    catatan text,
    snapshot jsonb NOT NULL,
    dibuat_pada timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX kurasi_keputusan_produk ON kurasi_keputusan (produk_id);
CREATE TRIGGER kurasi_keputusan_append_only
    BEFORE UPDATE OR DELETE ON kurasi_keputusan
    FOR EACH ROW EXECUTE FUNCTION tolak_ubah_append_only();

-- Idempotensi umum (POST mutasi): kunci per cakupan, respons tersimpan.
CREATE TABLE idempotensi (
    cakupan text NOT NULL,
    kunci text NOT NULL,
    pengguna_id uuid NOT NULL,
    status_http integer NOT NULL,
    respons jsonb NOT NULL,
    dibuat_pada timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (cakupan, kunci)
);
