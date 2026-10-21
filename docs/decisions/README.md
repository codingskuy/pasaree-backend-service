# Catatan Keputusan Arsitektur (ADR)

Cara menambah ADR:

1. Buat file baru `ADR-NNNN-judul-singkat.md` dengan nomor berurutan (tanpa melompat).
2. Satu keputusan per file. Format: Status | Tanggal | Konteks | Keputusan | Konsekuensi.
3. ADR bersifat immutable: jangan edit ADR lama. Bila keputusan berubah, tulis ADR baru yang menyatakan "Menggantikan ADR-NNNN".
4. Perubahan kontrak API/event wajib disertai ADR baru + bump versi.

Daftar:

- [ADR-0001 — Fondasi teknis](./ADR-0001-fondasi-teknis.md)
- [ADR-0002 — Keuangan: komisi, escrow, payout T+1, rekonsiliasi](./ADR-0002-keuangan-escrow-payout-rekonsiliasi.md)
