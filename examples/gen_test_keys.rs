/// Generate a Nostr keypair for testing purposes.
///
/// Usage: cargo run --example gen_test_keys
///
/// Outputs hex secret key, hex public key, nsec, and npub.
/// Use the nsec as MERCHANT_NSEC for local/test runs.

use nostr::ToBech32;

fn main() {
    let keys = nostr::Keys::generate();
    println!("SECRET_KEY (hex): {}", keys.secret_key().to_secret_hex());
    println!("PUBLIC_KEY (hex): {}", keys.public_key());
    println!(
        "NSEC: {}",
        keys.secret_key()
            .to_bech32()
            .expect("bech32 encode failed")
    );
    println!(
        "NPUB: {}",
        keys.public_key()
            .to_bech32()
            .expect("bech32 encode failed")
    );
}
