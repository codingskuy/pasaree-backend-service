# Changelog

## Belum dirilis

- feat(PSR-BE-005): checkout, pesanan, bayar idempoten, dan proksi trip Lumbung: migrasi `0003_pesanan.sql` (keranjang, pesanan, sub_pesanan per lapak, item snapshot, timeline, escrow awal, pembayaran, ulasan), state machine pesanan (DIPESAN ... SELESAI + DIBATALKAN/DIKEMBALIKAN) dengan test tiap transisi, stok dikunci 30 menit, konfirmasi lapak <= 2 jam jam kerja, `POST /pesanan/{id}/bayar` idempoten (`escrow_id` saat DIBAYAR), `GET /trip` cache 60 detik + flag `basi`, tugas berkala (batas bayar, konfirmasi kedaluwarsa, terima otomatis, masa retur).

- feat(PSR-BE-004): domain katalog, lapak, dan kurasi: migrasi `0002_katalog.sql` (produsen, lapak, kategori, produk, varian, media, `harga_riwayat` append-only, `kurasi_keputusan` beraudit, `idempotensi`), cek otomatis (judul 10-80 + berat/isi, kemiripan >= 85%, harga kelipatan Rp500, foto >= 800x800 <= 5 MB, kolom pangan), stok 0 -> `NONAKTIF_STOK_0` dan kembali `TAYANG`, perubahan harga > 20% memicu kurasi ulang ringan, ARSIP menggantikan hapus, katalog baca dengan cursor (default 20, maks 50), antrean dan keputusan kurasi.

- feat(PSR-BE-003): fondasi layanan: `AppState`, `AppError` (400/401/403/404/409/422/500), migrasi `sqlx::migrate!`, JWT akses 15 menit `aud=pasaree`, refresh 7 hari dengan reuse-detection, argon2, peran per route, klien Lumbung dengan service key; endpoint `/kesehatan`, `/auth/{daftar,login,refresh}`, `/saya`, `PATCH /web/admin/pengguna/{id}/peran`.

- docs: fondasi AI-dev v1.0.0 (AGENTS, ADR-0001, ARSITEKTUR-AI-DEV) — 2026-10-14.
- docs: kontrak OpenAPI v1.0.0 + Scalar reference (`docs/api/`).
