# Autentikasi Pasaree

## Wajib

- Skema `Authorization: Bearer <jwt>`.
- Klaim `aud` harus tepat `pasaree`. Contoh klaim:
  `{"sub": "pengguna-01", "aud": "pasaree", "iss": "pasaree-auth", "exp": 1893456000}`.
- Token lintas divisi tanpa `aud` pasaree ditolak dengan 401.

## Aturan

- Basis data terpisah `pasaree`. Layanan ini tidak membaca basis data divisi lain.
- Proksi ke Lumbung memakai token layanan internal, bukan token pengguna.
- Bayar wajib menyertakan kepala `idempotency-key`.

## Implementasi (PSR-BE-003)

- Akses: HS256, 15 menit, klaim `sub`, `aud=pasaree`, `iss=pasaree-auth`, `iat`, `exp`, `peran`. Tanpa leeway.
- Refresh: 7 hari, acak 256 bit, hanya hash SHA-256 disimpan; sekali pakai. Pemakaian ulang token lama mencabut seluruh keluarga token (reuse-detection).
- Peran: `PRODUSEN`, `OPERATOR`, `KURATOR`, `ADMIN`, `PEMBELI` (kolom `pengguna.peran`). Pendaftaran publik hanya membuat PEMBELI; admin mengubah peran lewat `PATCH /web/admin/pengguna/{id}/peran`. Peran tidak berhak = 403 `AKSES_DITOLAK`.
- Lingkungan: `JWT_SECRET` (>= 32 karakter), `LUMBUNG_BASE_URL`, `LUMBUNG_SERVICE_KEY` (lihat `.env.example`).
