# Changelog

## Belum dirilis

- feat(PSR-BE-004): domain katalog, lapak, dan kurasi: migrasi `0002_katalog.sql` (produsen, lapak, kategori, produk, varian, media, `harga_riwayat` append-only, `kurasi_keputusan` beraudit, `idempotensi`), cek otomatis (judul 10-80 + berat/isi, kemiripan >= 85%, harga kelipatan Rp500, foto >= 800x800 <= 5 MB, kolom pangan), stok 0 -> `NONAKTIF_STOK_0` dan kembali `TAYANG`, perubahan harga > 20% memicu kurasi ulang ringan, ARSIP menggantikan hapus, katalog baca dengan cursor (default 20, maks 50), antrean dan keputusan kurasi.

- feat(PSR-BE-003): fondasi layanan: `AppState`, `AppError` (400/401/403/404/409/422/500), migrasi `sqlx::migrate!`, JWT akses 15 menit `aud=pasaree`, refresh 7 hari dengan reuse-detection, argon2, peran per route, klien Lumbung dengan service key; endpoint `/kesehatan`, `/auth/{daftar,login,refresh}`, `/saya`, `PATCH /web/admin/pengguna/{id}/peran`.

- docs: fondasi AI-dev v1.0.0 (AGENTS, ADR-0001, ARSITEKTUR-AI-DEV) — 2026-10-14.
- docs: kontrak OpenAPI v1.0.0 + Scalar reference (`docs/api/`).
