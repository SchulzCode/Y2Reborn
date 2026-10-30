//! Operating-system randomness. Kept behind one function so the RNG crate can change.

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).expect("operating system randomness is unavailable");
    buf
}

pub fn random_vec(len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    getrandom::fill(&mut buf).expect("operating system randomness is unavailable");
    buf
}

pub fn random_u32() -> u32 {
    u32::from_le_bytes(random_bytes::<4>())
}
