/// Hard-coded Nostr keypair for testing and development.
///
/// DO NOT use this key for any real funds or production deployments.
/// Generated via `cargo run --example gen_test_keys`.
///
/// nsec: nsec1x3rxtm6waw62n2meqvx0zuyya9z9t9ru0sxlxva2ah2lt57w2agqz9773y
/// npub: npub1x2zp9zxzawpdnm6shtje3jqu4nv00906u8qec0w2h2s0r7dz5q6sf6juy0

/// Hex-encoded secret key (for `Keys::parse`).
pub const TEST_MERCHANT_SECRET_HEX: &str =
    "344665ef4eebb4a9ab79030cf17084e94455947c7c0df333aaedd5f5d3ce5750";

/// Hex-encoded public key.
pub const TEST_MERCHANT_PUBKEY_HEX: &str =
    "32841288c2eb82d9ef50bae598c81cacd8f795fae1c19c3dcabaa0f1f9a2a035";

/// Bech32-encoded secret key (nsec).
pub const TEST_MERCHANT_NSEC: &str =
    "nsec1x3rxtm6waw62n2meqvx0zuyya9z9t9ru0sxlxva2ah2lt57w2agqz9773y";

/// Bech32-encoded public key (npub).
pub const TEST_MERCHANT_NPUB: &str =
    "npub1x2zp9zxzawpdnm6shtje3jqu4nv00906u8qec0w2h2s0r7dz5q6sf6juy0";
