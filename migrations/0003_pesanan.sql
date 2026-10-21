-- PSR-BE-005: keranjang, pesanan multi-lapak, bayar idempoten, escrow awal, ulasan.

CREATE TABLE keranjang_item (
    pengguna_id uuid NOT NULL REFERENCES pengguna (id) ON DELETE CASCADE,
    varian_id uuid NOT NULL REFERENCES varian_produk (id),
    qty integer NOT NULL CHECK (qty BETWEEN 1 AND 99),
    harga_snapshot integer NOT NULL CHECK (harga_snapshot > 0),
    diperbarui_pada timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (pengguna_id, varian_id)
);

CREATE TABLE pesanan (
    id uuid PRIMARY KEY,
    pembeli_id uuid NOT NULL REFERENCES pengguna (id),
    trip_id text NOT NULL,
    titik_id text NOT NULL,
    voucher text,
    status text NOT NULL CHECK (status IN ('DIPESAN', 'DIBAYAR', 'DIKONFIRMASI_LAPAK', 'DISERAHKAN_KE_TRIP',
        'DITERIMA_PEMBELI', 'SELESAI', 'DIBATALKAN', 'DIKEMBALIKAN')),
    subtotal_idr bigint NOT NULL CHECK (subtotal_idr >= 0),
    ongkir_idr bigint NOT NULL CHECK (ongkir_idr >= 0),
    biaya_layanan_idr integer NOT NULL CHECK (biaya_layanan_idr >= 0),
    total_idr bigint NOT NULL CHECK (total_idr = subtotal_idr + ongkir_idr + biaya_layanan_idr),
    batas_bayar_hingga timestamptz NOT NULL,
    dibayar_pada timestamptz,
    metode_bayar text,
    dibuat_pada timestamptz NOT NULL DEFAULT now(),
    diperbarui_pada timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX pesanan_pembeli ON pesanan (pembeli_id, id DESC);
CREATE INDEX pesanan_status ON pesanan (status);

CREATE TABLE sub_pesanan (
    id uuid PRIMARY KEY,
    pesanan_id uuid NOT NULL REFERENCES pesanan (id),
    lapak_id uuid NOT NULL REFERENCES lapak (id),
    trip_id text NOT NULL,
    status text NOT NULL CHECK (status IN ('DIPESAN', 'DIBAYAR', 'DIKONFIRMASI_LAPAK', 'DISERAHKAN_KE_TRIP',
        'DITERIMA_PEMBELI', 'SELESAI', 'DIBATALKAN', 'DIKEMBALIKAN')),
    subtotal_idr bigint NOT NULL CHECK (subtotal_idr > 0),
    catatan text,
    batas_konfirmasi_hingga timestamptz,
    direspons_pada timestamptz,
    dikonfirmasi_pada timestamptz,
    diserahkan_pada timestamptz,
    diterima_pada timestamptz,
    selesai_pada timestamptz,
    bukti_serah_trip text,
    alasan_batal text,
    refund_idr bigint NOT NULL DEFAULT 0 CHECK (refund_idr >= 0),
    penalti_idr bigint NOT NULL DEFAULT 0 CHECK (penalti_idr >= 0),
    dibuat_pada timestamptz NOT NULL DEFAULT now(),
    diperbarui_pada timestamptz NOT NULL DEFAULT now(),
    UNIQUE (pesanan_id, lapak_id)
);
CREATE INDEX sub_pesanan_lapak ON sub_pesanan (lapak_id, status);
CREATE INDEX sub_pesanan_status ON sub_pesanan (status);

-- Snapshot item saat pesanan dibuat (harga tampil = harga bayar).
CREATE TABLE item_pesanan (
    id uuid PRIMARY KEY,
    sub_pesanan_id uuid NOT NULL REFERENCES sub_pesanan (id),
    varian_id uuid NOT NULL REFERENCES varian_produk (id),
    produk_id uuid NOT NULL REFERENCES produk (id),
    kategori_kode text NOT NULL,
    judul text NOT NULL,
    nama_varian text NOT NULL,
    sku text NOT NULL,
    harga_idr integer NOT NULL CHECK (harga_idr > 0),
    qty integer NOT NULL CHECK (qty > 0)
);
CREATE INDEX item_pesanan_sub ON item_pesanan (sub_pesanan_id);

CREATE TABLE pesanan_timeline (
    id uuid PRIMARY KEY,
    pesanan_id uuid NOT NULL REFERENCES pesanan (id),
    status text NOT NULL,
    waktu timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX pesanan_timeline_pesanan ON pesanan_timeline (pesanan_id, waktu);

-- Escrow: dana ditahan kasir sampai serah-terima trip; satu per pesanan.
CREATE TABLE escrow (
    id uuid PRIMARY KEY,
    pesanan_id uuid NOT NULL UNIQUE REFERENCES pesanan (id),
    jumlah_idr bigint NOT NULL CHECK (jumlah_idr > 0),
    refund_idr bigint NOT NULL DEFAULT 0 CHECK (refund_idr >= 0),
    penalti_idr bigint NOT NULL DEFAULT 0 CHECK (penalti_idr >= 0),
    status text NOT NULL DEFAULT 'TERTAHAN' CHECK (status IN ('TERTAHAN', 'DILEPAS_SEBAGIAN', 'DILEPAS', 'DIREFUND')),
    dibuat_pada timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE pembayaran (
    id uuid PRIMARY KEY,
    pesanan_id uuid NOT NULL UNIQUE REFERENCES pesanan (id),
    idempotency_key text NOT NULL,
    metode text NOT NULL CHECK (metode IN ('VA_BCA', 'QRIS', 'EWALLET_DANA')),
    jumlah_idr bigint NOT NULL CHECK (jumlah_idr > 0),
    referensi_kasir text NOT NULL,
    dibuat_pada timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE ulasan (
    id uuid PRIMARY KEY,
    pesanan_id uuid NOT NULL REFERENCES pesanan (id),
    produk_id uuid NOT NULL REFERENCES produk (id),
    pembeli_id uuid NOT NULL REFERENCES pengguna (id),
    bintang integer NOT NULL CHECK (bintang BETWEEN 1 AND 5),
    teks text,
    foto_url text[] NOT NULL DEFAULT '{}',
    dibuat_pada timestamptz NOT NULL DEFAULT now(),
    UNIQUE (pesanan_id, produk_id)
);
