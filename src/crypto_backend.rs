//! The one place that decides which implementation of SHA-256 and AES-256-CBC
//! the 7z `aes256` coder uses.
//!
//! Two backends are available, both of them `lzma-fast`'s:
//!
//! - **`aws-lc-crypto`** (on by default) — `aws-lc-rs` over AWS-LC. It is the
//!   scryer-media house default, shared with `lzma-fast` and `rarpar`, and it
//!   is what the numbers in `docs/benchmarking.md` were taken with.
//! - **`native-crypto`** — the RustCrypto crates (`sha2`, `aes`, `cbc`), for
//!   consumers who cannot have a C toolchain in their build.
//!
//! Cargo features are additive, so `native-crypto` cannot be expressed as
//! "turns `aws-lc-crypto` off". Instead **`native-crypto` takes precedence**:
//! whenever it is enabled this module selects RustCrypto, whether or not
//! AWS-LC is also compiled in. A build that wants no AWS-LC at all therefore
//! asks for `default-features = false` plus `native-crypto`, and a build that
//! enables both gets RustCrypto plus a test that the two agree.
//!
//! Enabling `aes256` with neither is a compile error rather than a silent
//! choice, because "which cryptography is in my binary" is not something a
//! crate should decide behind a consumer's back.
//!
//! Encryption (`compress` + `aes256`) is not routed through here: `lzma-fast`
//! exposes only the decrypting half, since decryption is all a 7z *reader*
//! needs, so the encoder in `encryption::aes` uses the RustCrypto `cbc`
//! encryptor directly. `docs/lzma-fast-requests.md` asks for the encrypting
//! counterpart; when it lands, the encoder moves here too.

#[cfg(all(feature = "aws-lc-crypto", not(feature = "native-crypto")))]
pub(crate) use lzma_fast::crypto::awslc::{Aes256Cbc, Sha256};
#[cfg(feature = "native-crypto")]
pub(crate) use lzma_fast::crypto::rustcrypto::{Aes256Cbc, Sha256};

#[cfg(not(any(feature = "aws-lc-crypto", feature = "native-crypto")))]
compile_error!(
    "the `aes256` feature needs a cryptography backend: enable `aws-lc-crypto` \
     (the default, AWS-LC) or `native-crypto` (RustCrypto, no C toolchain)"
);

/// Which backend this build selected. Only used by tests and diagnostics, but
/// a consumer wondering what is in their binary should be able to ask.
pub(crate) const BACKEND: &str = if cfg!(feature = "native-crypto") {
    "rustcrypto"
} else {
    "aws-lc"
};

/// Size of an AES block, for callers that chunk their input.
pub(crate) const AES_BLOCK_LEN: usize = 16;

/// The shape both backends' SHA-256 share, so the key derivation can be
/// written once and run against either one. `lzma-fast` exposes two concrete
/// types rather than a trait, and a trait defined here can be implemented for
/// both of them.
pub(crate) trait Sha256Like: Sized {
    /// A hash over no bytes yet.
    fn new() -> Self;
    /// Feeds the next bytes of the message.
    fn update(&mut self, data: &[u8]);
    /// Consumes the hash and returns the digest.
    fn finalize(self) -> [u8; 32];
}

macro_rules! impl_sha256_like {
    ($ty:path) => {
        impl Sha256Like for $ty {
            fn new() -> Self {
                <$ty>::new()
            }
            fn update(&mut self, data: &[u8]) {
                <$ty>::update(self, data);
            }
            fn finalize(self) -> [u8; 32] {
                <$ty>::finalize(self)
            }
        }
    };
}

#[cfg(feature = "aws-lc-crypto")]
impl_sha256_like!(lzma_fast::crypto::awslc::Sha256);
#[cfg(feature = "native-crypto")]
impl_sha256_like!(lzma_fast::crypto::rustcrypto::Sha256);

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// NIST SP 800-38A, F.2.6 (CBC-AES256.Decrypt): the four-block vector,
    /// which is also F.2.5's ciphertext.
    const NIST_KEY: &str = "603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4";
    const NIST_IV: &str = "000102030405060708090a0b0c0d0e0f";
    const NIST_CIPHERTEXT: &str = concat!(
        "f58c4c04d6e5f1ba779eabfb5f7bfbd6",
        "9cfc4e967edb808d679f777bc6702c7d",
        "39f23369a9d9bacfa530e26304231461",
        "b2eb05e2c39be9fcda6c19078c6a9d1b",
    );
    const NIST_PLAINTEXT: &str = concat!(
        "6bc1bee22e409f96e93d7e117393172a",
        "ae2d8a571e03ac9c9eb76fac45af8e51",
        "30c81c46a35ce411e5fbc1191a0a52ef",
        "f69f2445df4f9b17ad2b417be66c3710",
    );

    #[test]
    fn nist_cbc_aes256_decrypt() {
        let mut data = unhex(NIST_CIPHERTEXT);
        let mut cipher =
            Aes256Cbc::new(&unhex(NIST_KEY), &unhex(NIST_IV)).expect("key and iv are sized");
        cipher.decrypt(&mut data).expect("aligned ciphertext");
        assert_eq!(hex(&data), NIST_PLAINTEXT, "backend {BACKEND}");
    }

    /// The same vector fed one block at a time: CBC chaining has to survive
    /// being driven incrementally, which is exactly how the 7z reader drives
    /// it (it decrypts whatever the packed stream hands it).
    #[test]
    fn cbc_chaining_is_incremental() {
        let whole = unhex(NIST_CIPHERTEXT);
        let mut cipher =
            Aes256Cbc::new(&unhex(NIST_KEY), &unhex(NIST_IV)).expect("key and iv are sized");
        let mut out = Vec::new();
        for block in whole.chunks(AES_BLOCK_LEN) {
            let mut block = block.to_vec();
            cipher.decrypt(&mut block).expect("aligned ciphertext");
            out.extend_from_slice(&block);
        }
        assert_eq!(hex(&out), NIST_PLAINTEXT, "backend {BACKEND}");
    }

    #[test]
    fn sha256_known_vectors() {
        let mut sha = Sha256::new();
        sha.update(b"abc");
        assert_eq!(
            hex(&sha.finalize()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "backend {BACKEND}"
        );

        let mut sha = Sha256::new();
        for _ in 0..1000 {
            sha.update(&[b'a'; 1000]);
        }
        assert_eq!(
            hex(&sha.finalize()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
            "backend {BACKEND}"
        );
    }

    /// When both backends are compiled in, they must agree — on raw AES-CBC,
    /// on SHA-256, and on the 7z key derivation that sits on top of it,
    /// including the two cycle counts `7zAes.c` treats specially.
    #[cfg(all(feature = "aws-lc-crypto", feature = "native-crypto"))]
    mod differential {
        use lzma_fast::crypto::{awslc, rustcrypto};

        use crate::encryption::derive_key_with;

        fn sample(len: usize, seed: u64) -> Vec<u8> {
            let mut state = seed | 1;
            (0..len)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (state >> 24) as u8
                })
                .collect()
        }

        #[test]
        fn backends_agree_on_cbc() {
            let key = sample(32, 11);
            let iv = sample(16, 22);
            for blocks in [1usize, 2, 5, 64] {
                let plain = sample(blocks * 16, 33 + blocks as u64);

                let mut a = plain.clone();
                awslc::Aes256Cbc::new(&key, &iv)
                    .expect("sized")
                    .decrypt(&mut a)
                    .expect("aligned");

                let mut b = plain.clone();
                rustcrypto::Aes256Cbc::new(&key, &iv)
                    .expect("sized")
                    .decrypt(&mut b)
                    .expect("aligned");

                assert_eq!(a, b, "backends disagree on {blocks} block(s)");
            }
        }

        #[test]
        fn backends_agree_on_sha256() {
            for len in [0usize, 1, 55, 56, 64, 65, 1000] {
                let message = sample(len, 77 + len as u64);

                let mut a = awslc::Sha256::new();
                a.update(&message);
                let mut b = rustcrypto::Sha256::new();
                b.update(&message);

                assert_eq!(
                    a.finalize(),
                    b.finalize(),
                    "backends disagree on {len} bytes"
                );
            }
        }

        /// `7zAes.c`'s derivation, both backends. The two special cycle counts
        /// never reach a backend — `0x3F` means "the key is the salt and the
        /// password themselves" and `>= 0x40` is rejected outright, both in
        /// `encryption::aes::get_aes_key` before any hashing — so they are
        /// covered by that module's own tests instead.
        #[test]
        fn backends_agree_on_the_7z_key_derivation() {
            let salt = b"\x01\x02\x03\x04\x05\x06\x07\x08";
            let password = b"p\0a\0s\0s\0w\0o\0r\0d\0";
            for cycles in [0u8, 1, 4, 8, 12] {
                let a = derive_key_with::<awslc::Sha256>(cycles, salt, password);
                let b = derive_key_with::<rustcrypto::Sha256>(cycles, salt, password);
                assert_eq!(a, b, "backends disagree at cycles {cycles}");
            }
        }
    }
}
