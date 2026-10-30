# Y2Reborn working rules

Write code to [the coding standard](https://github.com/SchulzCode/Y2Linux/blob/main/docs/CODING-STANDARD.md)
(Y2Linux `docs/CODING-STANDARD.md`; it covers Reborn and the platform alike). Before a commit: `cargo fmt --all`,
`cargo clippy --workspace --all-targets` without a warning, `cargo test --workspace` without a failure, then Y2Linux's
`tools/development/platform-gate.sh`, which runs Reborn's contract test and the code-quality ratchet over Reborn.

Records from the platform are read in their `y2p-contract` types; the contract crate is edited in Y2Linux only and
copied here with `release.py sync-contract`.

Preserve the existing configured Git author/committer identity. Do not modify identity settings or add model
attribution.
