//! Hashcash proof-of-work for the client-token and login5 services (spec section 5.5).

use std::time::{Duration, Instant};

use sha1::{Digest, Sha1};

/// Find a 16-byte suffix such that `SHA1(prefix || suffix)` ends in at least `length`
/// zero bits (measured on bytes 12..20 as a big-endian i64). `ctx` seeds the search:
/// login5 passes its `login_context`, the client-token service passes nothing.
/// Returns the suffix and how long it took, or `None` after `timeout`.
pub fn solve(ctx: &[u8], prefix: &[u8], length: i32, timeout: Duration) -> Option<([u8; 16], Duration)> {
    let start = Instant::now();
    let seed = Sha1::digest(ctx);
    let target = i64::from_be_bytes(seed[12..20].try_into().expect("8 bytes"));

    let mut counter: i64 = 0;
    loop {
        if start.elapsed() >= timeout {
            return None;
        }
        let mut suffix = [0u8; 16];
        suffix[..8].copy_from_slice(&target.wrapping_add(counter).to_be_bytes());
        suffix[8..].copy_from_slice(&counter.to_be_bytes());
        if check(prefix, &suffix, length) {
            return Some((suffix, start.elapsed()));
        }
        counter += 1;
    }
}

pub fn check(prefix: &[u8], suffix: &[u8], length: i32) -> bool {
    let mut hasher = Sha1::new();
    hasher.update(prefix);
    hasher.update(suffix);
    let digest = hasher.finalize();
    let tail = i64::from_be_bytes(digest[12..20].try_into().expect("8 bytes"));
    tail.trailing_zeros() >= length.max(0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn solves_the_client_token_style_vector() {
        let prefix = hex::decode("a1b2c3d4").unwrap();
        let (suffix, _) = solve(&[], &prefix, 10, Duration::from_secs(30)).unwrap();
        assert_eq!(hex::encode(suffix), "95601890afd808910000000000000188");
        assert!(check(&prefix, &suffix, 10));
    }

    #[test]
    fn solves_the_login5_style_vector() {
        let (suffix, _) = solve(b"login-context", b"housecat", 12, Duration::from_secs(30)).unwrap();
        assert_eq!(hex::encode(suffix), "0a425b56e8c1ffdc000000000000174a");
        assert!(check(b"housecat", &suffix, 12));
    }

    #[test]
    fn check_rejects_a_wrong_suffix() {
        let prefix = hex::decode("a1b2c3d4").unwrap();
        let mut suffix = hex::decode("95601890afd808910000000000000188").unwrap();
        suffix[15] ^= 1;
        assert!(!check(&prefix, &suffix, 10));
    }

    #[test]
    fn length_zero_is_solved_immediately() {
        let (suffix, took) = solve(b"", b"x", 0, Duration::from_secs(1)).unwrap();
        assert_eq!(&suffix[8..], &[0u8; 8], "counter zero");
        assert!(took < Duration::from_secs(1));
    }

    #[test]
    fn gives_up_after_the_timeout() {
        assert!(solve(b"", b"x", 60, Duration::from_millis(50)).is_none());
    }
}
