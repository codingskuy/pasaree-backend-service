-- PSR-BE-003: pengguna + refresh token (reuse-detection per keluarga token).
CREATE TABLE pengguna (
    id uuid PRIMARY KEY,
    email text NOT NULL,
    kata_sandi_hash text NOT NULL,
    nama text NOT NULL DEFAULT '',
    peran text NOT NULL CHECK (peran IN ('PRODUSEN', 'OPERATOR', 'KURATOR', 'ADMIN', 'PEMBELI')),
    dibuat_pada timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX pengguna_email_unik ON pengguna (lower(email));

CREATE TABLE refresh_token (
    id uuid PRIMARY KEY,
    pengguna_id uuid NOT NULL REFERENCES pengguna (id) ON DELETE CASCADE,
    keluarga uuid NOT NULL,
    token_hash text NOT NULL UNIQUE,
    kedaluwarsa timestamptz NOT NULL,
    dipakai_pada timestamptz,
    dicabut_pada timestamptz,
    dibuat_pada timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX refresh_token_keluarga ON refresh_token (keluarga);
