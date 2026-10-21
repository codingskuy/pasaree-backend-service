//! ID publik = ULID string; disimpan sebagai `uuid` (128 bit yang sama, urut waktu).

use uuid::Uuid;

pub fn baru() -> Uuid {
    Uuid::from_u128(ulid::Ulid::new().0)
}

/// Uuid -> ULID string (26 karakter).
pub fn ke_ulid(id: Uuid) -> String {
    ulid::Ulid(id.as_u128()).to_string()
}

/// ULID string -> Uuid. Menerima awalan opsional (`PSR-`, `ESC-`).
pub fn dari_ulid(s: &str, awalan: &str) -> Option<Uuid> {
    let isi = if awalan.is_empty() {
        s
    } else {
        s.strip_prefix(awalan)?
    };
    ulid::Ulid::from_string(isi)
        .ok()
        .map(|u| Uuid::from_u128(u.0))
}

pub fn dengan_awalan(awalan: &str, id: Uuid) -> String {
    format!("{awalan}{}", ke_ulid(id))
}

#[cfg(test)]
mod uji {
    use super::*;

    #[test]
    fn bolak_balik() {
        let id = baru();
        let s = ke_ulid(id);
        assert_eq!(s.len(), 26);
        assert_eq!(dari_ulid(&s, ""), Some(id));
        let p = dengan_awalan("PSR-", id);
        assert_eq!(dari_ulid(&p, "PSR-"), Some(id));
        assert_eq!(dari_ulid(&s, "PSR-"), None);
        assert_eq!(dari_ulid("bukan-ulid", ""), None);
    }
}
