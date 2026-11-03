//! Stateless signed session cookies.
//!
//! A session is an HMAC-SHA256-signed `v1|site|username|expiry` payload. No
//! server-side session table is needed, so sessions survive request routing
//! across workers; they are invalidated by restarting with a new secret or by
//! waiting for the expiry.

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Name of the session cookie.
pub const COOKIE_NAME: &str = "webprint_session";

/// An authenticated team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub site: String,
    pub username: String,
    /// Unix timestamp at which the session stops being valid.
    pub expires_at: i64,
}

/// The key used to sign sessions.
#[derive(Clone)]
pub struct SessionKey([u8; 32]);

impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionKey(..)")
    }
}

impl SessionKey {
    /// Derives a key from the configured secret, or generates a random one.
    pub fn new(secret: Option<&str>) -> Self {
        match secret {
            Some(s) if !s.is_empty() => {
                let digest = Sha256::digest(s.as_bytes());
                let mut key = [0u8; 32];
                key.copy_from_slice(&digest);
                Self(key)
            }
            _ => Self::random(),
        }
    }

    pub fn random() -> Self {
        Self(rand::random())
    }

    fn mac(&self, payload: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(&self.0).expect("hmac accepts any key length");
        mac.update(payload);
        mac.finalize().into_bytes().to_vec()
    }

    /// Issues a cookie value for `site`/`username`, valid until `expires_at`.
    pub fn sign(&self, site: &str, username: &str, expires_at: i64) -> String {
        let payload = format!("v1|{site}|{username}|{expires_at}");
        let tag = self.mac(payload.as_bytes());
        format!("{}.{}", B64.encode(payload), B64.encode(tag))
    }

    /// Verifies a cookie value. Returns `None` when it is malformed, has a bad
    /// signature, or has expired at `now`.
    pub fn verify(&self, value: &str, now: i64) -> Option<Session> {
        let (payload_b64, tag_b64) = value.split_once('.')?;
        let payload = B64.decode(payload_b64).ok()?;
        let tag = B64.decode(tag_b64).ok()?;

        let expected = self.mac(&payload);
        let ok: bool = expected.ct_eq(&tag).into();
        if !ok {
            return None;
        }

        let payload = String::from_utf8(payload).ok()?;
        let mut parts = payload.split('|');
        if parts.next()? != "v1" {
            return None;
        }
        let site = parts.next()?.to_string();
        let username = parts.next()?.to_string();
        let expires_at: i64 = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        if expires_at <= now {
            return None;
        }

        Some(Session {
            site,
            username,
            expires_at,
        })
    }
}

/// Builds a `Set-Cookie` header value for a session.
///
/// `base_path` is the public prefix the server is served under (empty at the
/// root). Scoping the cookie to it keeps it off the rest of the host when
/// webprint shares a domain with something else.
pub fn session_cookie(value: &str, max_age: i64, secure: bool, base_path: &str) -> String {
    let mut c = format!(
        "{COOKIE_NAME}={value}; Path={}; Max-Age={max_age}; HttpOnly; SameSite=Lax",
        cookie_path(base_path)
    );
    if secure {
        c.push_str("; Secure");
    }
    c
}

/// Builds a `Set-Cookie` header value that clears the session.
pub fn clear_cookie(secure: bool, base_path: &str) -> String {
    let mut c = format!(
        "{COOKIE_NAME}=; Path={}; Max-Age=0; HttpOnly; SameSite=Lax",
        cookie_path(base_path)
    );
    if secure {
        c.push_str("; Secure");
    }
    c
}

/// A cookie `Path` is never empty: the root is `/`.
fn cookie_path(base_path: &str) -> &str {
    if base_path.is_empty() {
        "/"
    } else {
        base_path
    }
}

/// Extracts one cookie value from a `Cookie` header.
pub fn cookie_value(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == name).then(|| v.trim().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_762_000_000;

    #[test]
    fn round_trips_a_session() {
        let key = SessionKey::new(Some("secret"));
        let cookie = key.sign("main-hall", "team1", NOW + 86_400);
        let session = key.verify(&cookie, NOW).expect("valid");
        assert_eq!(session.site, "main-hall");
        assert_eq!(session.username, "team1");
        assert_eq!(session.expires_at, NOW + 86_400);
    }

    #[test]
    fn a_secret_produces_a_stable_key_across_restarts() {
        let a = SessionKey::new(Some("shared"));
        let b = SessionKey::new(Some("shared"));
        let cookie = a.sign("s", "u", NOW + 10);
        assert!(b.verify(&cookie, NOW).is_some());
    }

    #[test]
    fn a_different_key_rejects_the_cookie() {
        let cookie = SessionKey::new(Some("one")).sign("s", "u", NOW + 10);
        assert!(SessionKey::new(Some("two")).verify(&cookie, NOW).is_none());
        assert!(SessionKey::random().verify(&cookie, NOW).is_none());
    }

    #[test]
    fn expired_sessions_are_rejected() {
        let key = SessionKey::new(Some("secret"));
        let cookie = key.sign("s", "u", NOW);
        assert!(key.verify(&cookie, NOW).is_none());
        assert!(key.verify(&cookie, NOW - 1).is_some());
    }

    #[test]
    fn tampering_is_detected() {
        let key = SessionKey::new(Some("secret"));
        let cookie = key.sign("main-hall", "team1", NOW + 86_400);
        let (payload, tag) = cookie.split_once('.').unwrap();

        // Re-sign a different identity with the original tag.
        let forged = format!("{}.{tag}", B64.encode("v1|main-hall|admin|99999999999"));
        assert!(key.verify(&forged, NOW).is_none());

        // Flip a bit in the signature.
        let mut bad_tag = B64.decode(tag).unwrap();
        bad_tag[0] ^= 1;
        assert!(key
            .verify(&format!("{payload}.{}", B64.encode(bad_tag)), NOW)
            .is_none());

        for malformed in ["", ".", "x", "x.y", "....", payload] {
            assert!(key.verify(malformed, NOW).is_none(), "{malformed:?}");
        }
    }

    #[test]
    fn extra_payload_fields_are_rejected() {
        let key = SessionKey::new(Some("secret"));
        let payload = "v1|s|u|99999999999|extra";
        let tag = key.mac(payload.as_bytes());
        let cookie = format!("{}.{}", B64.encode(payload), B64.encode(tag));
        assert!(key.verify(&cookie, NOW).is_none());

        let payload = "v2|s|u|99999999999";
        let tag = key.mac(payload.as_bytes());
        let cookie = format!("{}.{}", B64.encode(payload), B64.encode(tag));
        assert!(key.verify(&cookie, NOW).is_none());
    }

    #[test]
    fn cookie_headers_are_well_formed() {
        let set = session_cookie("abc", 86_400, false, "");
        assert!(set.starts_with("webprint_session=abc; Path=/; Max-Age=86400"));
        assert!(set.contains("HttpOnly"));
        assert!(set.contains("SameSite=Lax"));
        assert!(!set.contains("Secure"));
        assert!(session_cookie("abc", 10, true, "").contains("; Secure"));
        assert!(clear_cookie(false, "").contains("Max-Age=0"));
        assert!(clear_cookie(false, "").contains("Path=/;"));
    }

    #[test]
    fn cookies_are_scoped_to_the_public_path_prefix() {
        // The browser sees the proxied path, so that is what the cookie must
        // carry; otherwise it is either not sent back or sent site-wide.
        let set = session_cookie("abc", 86_400, false, "/print");
        assert!(set.contains("Path=/print;"), "{set}");
        assert!(clear_cookie(false, "/print").contains("Path=/print;"));
    }

    #[test]
    fn cookie_parsing_picks_the_right_pair() {
        assert_eq!(
            cookie_value("a=1; webprint_session=xyz; b=2", COOKIE_NAME).as_deref(),
            Some("xyz")
        );
        assert_eq!(
            cookie_value("webprint_session=xyz", COOKIE_NAME).as_deref(),
            Some("xyz")
        );
        assert_eq!(cookie_value("other=1", COOKIE_NAME), None);
        assert_eq!(cookie_value("", COOKIE_NAME), None);
    }
}
