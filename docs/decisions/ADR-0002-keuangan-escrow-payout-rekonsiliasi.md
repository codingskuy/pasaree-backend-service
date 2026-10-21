# ADR-0002 — Keuangan: komisi, escrow, payout T+1, dan rekonsiliasi

Status: Diusulkan | Tanggal: 2026-10-21 | Tiket: PSR-BE-005, PSR-BE-006

## Konteks

BRD/FSD/SRS v1.0.0 masih templat; aturan uang bersumber dari `keuangan/10-komisi-transparan.md`, `produk/10-alur-beli.md`, ADR-0001, dan acceptance criteria tiket. Beberapa titik dokumen saling bertentangan atau diam, sehingga implementasi memilih salah satu dan mencatatnya di sini agar dapat ditinjau manusia.

## Keputusan

1. **Pelepasan escrow terjadi saat serah-terima trip tercatat** (verifikasi `kode_serah` ke Lumbung), bukan saat pembeli konfirmasi terima. Alasan: komisi-transparan §3 dan AC #6 menyebut serah-terima; kontrak KMP menyebut konfirmasi terima. Konfirmasi terima hanya memajukan status pesanan.
2. **Komisi dibekukan saat bayar** per baris item (`tarif_persen`, `komisi_idr` pada `item_pesanan`) memakai tarif yang berlaku pada hari bayar (WIB). Rumus `floor(harga x qty x tarif / 100)` dalam integer IDR. Promo lapak baru (5%, 30 hari sejak `produk_pertama_tayang_pada`) mengalahkan tarif lain termasuk hampers.
3. **Komisi tidak pernah masuk total pembeli.** Total = subtotal + ongkir per trip + biaya layanan Rp2.000. Payout lapak = subtotal sub-pesanan - komisi; ongkir diteruskan ke kas trip Lumbung dan biaya layanan menjadi pendapatan operasional Pasaree.
4. **Riwayat tarif append-only** (`tarif_komisi`, trigger menolak UPDATE/DELETE). Perubahan tarif default hanya lewat `PATCH /web/admin/komisi`, `berlaku_mulai` minimal 14 hari dari hari ini (WIB).
5. **Payout dieksekusi Lumbung**, bukan Pasaree. Pasaree menjadwalkan baris `payout` (T+1 hari kerja Sen-Jum; libur nasional belum dimodelkan), lalu mengirim instruksi idempoten (`payout_id`) ke Lumbung memakai service key. Kegagalan membuat payout tetap `MENUNGGU` dan dicoba lagi tiap menit.
6. **Refund**: sebelum payout dikirim dibatalkan dari escrow; setelah payout dikirim dicatat `potongan_lapak` yang dipotong dari payout berikutnya (sebagian bila perlu). Penalti 10% (floor) pembatalan pembeli pasca-konfirmasi diteruskan ke lapak tanpa komisi.
7. **Rekonsiliasi harian** (WIB, otomatis untuk kemarin) membandingkan baris Pasaree dengan `GET /internal/pasaree/rekonsiliasi` Lumbung. Tiket prioritas tinggi bila selisih mutlak (subtotal + komisi + ongkir + payout) lebih dari Rp1.000, baris tidak tercatat di Lumbung lebih dari 2x24 jam, atau payout belum terbayar lebih dari 2x24 jam setelah jadwal.
8. **Audit trail 2 tahun**: tabel `audit_keuangan` append-only; baris tidak dapat dihapus sebelum `retensi_hingga` (now + 2 tahun). Tidak ada penghapusan otomatis.
9. **Kontrak Lumbung diasumsikan** (belum ada di OpenAPI Lumbung): `GET /internal/pasaree/trip`, `POST /internal/pasaree/serah-terima/verifikasi`, `POST /internal/pasaree/payout`, `GET /internal/pasaree/rekonsiliasi`. Perlu disepakati dengan divisi Lumbung sebelum integrasi nyata.

## Konsekuensi

- Penyimpangan dari keputusan 1 (escrow dilepas saat konfirmasi terima) memerlukan ADR baru dan perubahan `keuangan::lepas_escrow_sub`.
- Hari libur nasional, notifikasi lapak/admin, unduhan CSV laporan lapak, dan rekening payout lengkap (dikelola Lumbung) belum diimplementasikan.
- ADR berikutnya (mis. tooling uji) memakai nomor 0003.
