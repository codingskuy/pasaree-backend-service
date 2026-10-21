-- PSR-BE-006: komisi, riwayat tarif append-only, payout T+1, potongan, rekonsiliasi, audit keuangan 2 tahun.

-- Riwayat tarif (append-only): tarif_persen, berlaku_mulai, diubah_oleh, alasan.
CREATE TABLE tarif_komisi (
    id uuid PRIMARY KEY,
    kode text NOT NULL CHECK (kode IN ('DEFAULT', 'HAMPERS', 'LAPAK_BARU')),
    tarif_persen integer NOT NULL CHECK (tarif_persen BETWEEN 0 AND 100),
    berlaku_mulai date NOT NULL,
    diumumkan_pada timestamptz NOT NULL DEFAULT now(),
    diubah_oleh uuid REFERENCES pengguna (id),
    alasan text NOT NULL,
    dibuat_pada timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX tarif_komisi_kode ON tarif_komisi (kode, berlaku_mulai DESC);
CREATE TRIGGER tarif_komisi_append_only
    BEFORE UPDATE OR DELETE ON tarif_komisi
    FOR EACH ROW EXECUTE FUNCTION tolak_ubah_append_only();
INSERT INTO tarif_komisi (id, kode, tarif_persen, berlaku_mulai, diumumkan_pada, alasan) VALUES
    (gen_random_uuid(), 'DEFAULT', 8, DATE '2026-01-01', TIMESTAMPTZ '2026-01-01 00:00:00+00', 'Tarif awal Varian 1'),
    (gen_random_uuid(), 'HAMPERS', 10, DATE '2026-01-01', TIMESTAMPTZ '2026-01-01 00:00:00+00', 'Tarif awal Varian 1 (paket-hampers)'),
    (gen_random_uuid(), 'LAPAK_BARU', 5, DATE '2026-01-01', TIMESTAMPTZ '2026-01-01 00:00:00+00', 'Promo pembuka lapak baru, 30 hari sejak produk pertama tayang');

-- Komisi per baris dibekukan saat bayar (tarif yang berlaku saat itu).
ALTER TABLE item_pesanan ADD COLUMN tarif_persen integer, ADD COLUMN komisi_idr bigint;
ALTER TABLE sub_pesanan ADD COLUMN komisi_idr bigint;

-- Payout lapak: dieksekusi Lumbung T+1 hari kerja setelah escrow dilepas (serah-terima trip).
CREATE TABLE payout (
    id uuid PRIMARY KEY,
    sub_pesanan_id uuid NOT NULL UNIQUE REFERENCES sub_pesanan (id),
    pesanan_id uuid NOT NULL REFERENCES pesanan (id),
    lapak_id uuid NOT NULL REFERENCES lapak (id),
    jenis text NOT NULL DEFAULT 'PENJUALAN' CHECK (jenis IN ('PENJUALAN', 'PENALTI')),
    subtotal_idr bigint NOT NULL CHECK (subtotal_idr >= 0),
    komisi_idr bigint NOT NULL CHECK (komisi_idr >= 0),
    jumlah_idr bigint NOT NULL CHECK (jumlah_idr >= 0),
    potongan_idr bigint NOT NULL DEFAULT 0 CHECK (potongan_idr >= 0),
    potongan_diterapkan boolean NOT NULL DEFAULT false,
    status text NOT NULL DEFAULT 'MENUNGGU' CHECK (status IN ('MENUNGGU', 'MENGIRIM', 'DIKIRIM', 'TERBAYAR', 'DIBATALKAN')),
    jadwal_payout date NOT NULL,
    percobaan integer NOT NULL DEFAULT 0,
    mengirim_sejak timestamptz,
    gagal_terakhir text,
    lumbung_ref text,
    dilepas_pada timestamptz NOT NULL DEFAULT now(),
    dieksekusi_pada timestamptz
);
CREATE INDEX payout_jatuh_tempo ON payout (status, jadwal_payout);
CREATE INDEX payout_lapak ON payout (lapak_id, status);

-- Refund setelah payout dipotong dari payout berikutnya (sisa_idr berkurang saat diterapkan).
CREATE TABLE potongan_lapak (
    id uuid PRIMARY KEY,
    lapak_id uuid NOT NULL REFERENCES lapak (id),
    sub_pesanan_id uuid NOT NULL UNIQUE REFERENCES sub_pesanan (id),
    jumlah_idr bigint NOT NULL CHECK (jumlah_idr > 0),
    sisa_idr bigint NOT NULL CHECK (sisa_idr >= 0),
    alasan text NOT NULL,
    dibuat_pada timestamptz NOT NULL DEFAULT now()
);

-- Audit trail keuangan: append-only; baris tidak dapat dihapus sebelum `retensi_hingga` (2 tahun).
CREATE TABLE audit_keuangan (
    id uuid PRIMARY KEY,
    waktu timestamptz NOT NULL DEFAULT now(),
    jenis text NOT NULL,
    pesanan_id uuid,
    sub_pesanan_id uuid,
    lapak_id uuid,
    jumlah_idr bigint,
    data jsonb NOT NULL DEFAULT '{}',
    retensi_hingga timestamptz NOT NULL DEFAULT (now() + interval '2 years')
);
CREATE INDEX audit_keuangan_pesanan ON audit_keuangan (pesanan_id, waktu);
CREATE INDEX audit_keuangan_waktu ON audit_keuangan (waktu);

CREATE FUNCTION tolak_audit_ubah_hapus() RETURNS trigger AS $$
BEGIN
    IF TG_OP = 'UPDATE' THEN
        RAISE EXCEPTION 'audit_keuangan append-only' USING ERRCODE = '23000';
    END IF;
    IF OLD.retensi_hingga > now() THEN
        RAISE EXCEPTION 'audit_keuangan wajib disimpan sampai %', OLD.retensi_hingga USING ERRCODE = '23000';
    END IF;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER audit_keuangan_guard
    BEFORE UPDATE OR DELETE ON audit_keuangan
    FOR EACH ROW EXECUTE FUNCTION tolak_audit_ubah_hapus();

-- Rekonsiliasi harian Pasaree-Lumbung.
CREATE TABLE rekonsiliasi_jalan (
    id uuid PRIMARY KEY,
    tanggal date NOT NULL UNIQUE,
    status text NOT NULL CHECK (status IN ('SELESAI', 'GAGAL_LUMBUNG')),
    jumlah_baris integer NOT NULL DEFAULT 0,
    jumlah_tiket integer NOT NULL DEFAULT 0,
    dibuat_pada timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE rekonsiliasi_baris (
    id uuid PRIMARY KEY,
    jalan_id uuid NOT NULL REFERENCES rekonsiliasi_jalan (id) ON DELETE CASCADE,
    pesanan_id uuid NOT NULL,
    escrow_id uuid,
    subtotal_idr bigint NOT NULL,
    komisi_idr bigint NOT NULL,
    ongkir_idr bigint NOT NULL,
    payout_idr bigint NOT NULL,
    status text NOT NULL,
    status_lumbung text,
    selisih_idr bigint
);
CREATE INDEX rekonsiliasi_baris_jalan ON rekonsiliasi_baris (jalan_id);

CREATE TABLE tiket_rekonsiliasi (
    id uuid PRIMARY KEY,
    jenis text NOT NULL CHECK (jenis IN ('SELISIH', 'GANTUNG')),
    prioritas text NOT NULL DEFAULT 'TINGGI',
    pesanan_id uuid NOT NULL,
    escrow_id uuid,
    selisih_idr bigint NOT NULL DEFAULT 0,
    rincian jsonb NOT NULL DEFAULT '{}',
    status text NOT NULL DEFAULT 'TERBUKA' CHECK (status IN ('TERBUKA', 'SELESAI')),
    admin_diberi_tahu_pada timestamptz NOT NULL DEFAULT now(),
    lapak_diberi_tahu_pada timestamptz,
    dibuat_pada timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX tiket_rekonsiliasi_terbuka ON tiket_rekonsiliasi (pesanan_id, jenis) WHERE status = 'TERBUKA';
