//! Diffie-Hellman for the access-point handshake (spec section 5.1, step 3).

use num_bigint::BigUint;

use crate::consts;
use crate::random;

pub struct DhKeys {
    private: BigUint,
    public: BigUint,
}

impl DhKeys {
    pub fn random() -> Self {
        Self::from_private(BigUint::from_bytes_be(&random::random_vec(consts::DH_PRIVATE_KEY_BYTES)))
    }

    pub(crate) fn from_private(private: BigUint) -> Self {
        let prime = BigUint::from_bytes_be(&consts::DH_PRIME);
        let public = BigUint::from(consts::DH_GENERATOR).modpow(&private, &prime);
        DhKeys { private, public }
    }

    /// Our public key, big-endian without leading zero padding (what the server expects in `gc`).
    pub fn public_key(&self) -> Vec<u8> {
        self.public.to_bytes_be()
    }

    /// `remote_public ^ private mod p`, big-endian without padding, exactly as librespot
    /// feeds it into key derivation.
    pub fn shared_secret(&self, remote_public: &[u8]) -> Vec<u8> {
        let prime = BigUint::from_bytes_be(&consts::DH_PRIME);
        BigUint::from_bytes_be(remote_public).modpow(&self.private, &prime).to_bytes_be()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prime_matches_rfc_2409_group_1() {
        let expected = concat!(
            "ffffffffffffffffc90fdaa22168c234c4c6628b80dc1cd129024e088a67cc74",
            "020bbea63b139b22514a08798e3404ddef9519b3cd3a431b302b0a6df25f1437",
            "4fe1356d6d51c245e485b576625e7ec6f44c42e9a63a3620ffffffffffffffff"
        );
        assert_eq!(hex::encode(crate::consts::DH_PRIME), expected);
    }

    #[test]
    fn small_private_keys_give_known_public_keys() {
        // 2^1 mod p = 2, 2^3 mod p = 8
        assert_eq!(DhKeys::from_private(BigUint::from(1u32)).public_key(), vec![2]);
        assert_eq!(DhKeys::from_private(BigUint::from(3u32)).public_key(), vec![8]);
    }

    #[test]
    fn both_sides_derive_the_same_secret() {
        let a = DhKeys::random();
        let b = DhKeys::random();
        let ab = a.shared_secret(&b.public_key());
        let ba = b.shared_secret(&a.public_key());
        assert_eq!(ab, ba);
        assert!(!ab.is_empty() && ab.len() <= 96);
    }

    #[test]
    fn random_keys_differ_and_fit_the_group() {
        let a = DhKeys::random();
        let b = DhKeys::random();
        assert_ne!(a.public_key(), b.public_key());
        assert!(a.public_key().len() <= 96);
    }
}
