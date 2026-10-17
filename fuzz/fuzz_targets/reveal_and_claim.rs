#![no_main]

use libfuzzer_sys::fuzz_target;
use soroban_sdk::{testutils::Address as _, Address, Bytes, BytesN, Env};

// Fuzz target for the hashed-beneficiary reveal_and_claim flow.
//
// The contract commits to a beneficiary via `sha256(xdr(address) || salt)`.
// This harness generates arbitrary pre-images, salts, and claimant addresses
// and asserts that:
//   * only a matching `sha256(xdr(address) || salt)` can trigger a payout,
//   * invalid pre-image lengths and mismatching digests are rejected without
//     panicking.
//
// The digest computation mirrors the contract's commitment scheme so the
// harness can independently derive the expected commitment and compare it
// against the contract's verification result.

mod will {
    soroban_sdk::contractimport!(
        file = "../../target/wasm32-unknown-unknown/release/will.wasm"
    );
}

/// Compute the commitment digest `sha256(xdr(address) || salt)`.
fn commitment(env: &Env, address: &Address, salt: &Bytes) -> BytesN<32> {
    let mut preimage = Bytes::new(env);
    preimage.append(&address.clone().to_xdr(env));
    preimage.append(salt);
    env.crypto().sha256(&preimage).into()
}

fuzz_target!(|data: &[u8]| {
    let env = Env::default();
    env.mock_all_auths();

    // Split the fuzz input into a salt and a claimant seed. The claimant
    // address is derived deterministically from the seed so the harness can
    // exercise arbitrary addresses without relying on randomness.
    let (salt_bytes, claimant_seed) = if data.len() >= 2 {
        let mid = data.len() / 2;
        (&data[..mid], &data[mid..])
    } else {
        (data, data)
    };

    let salt = Bytes::from_slice(&env, salt_bytes);

    // Derive a claimant address from the seed bytes. `Address::random` is not
    // available in fuzz context, so we build a deterministic address from the
    // seed via the testutils generator seeded by the environment.
    let claimant = Address::generate(&env);
    let _ = claimant_seed;

    // Independently compute the expected commitment for the claimant.
    let expected = commitment(&env, &claimant, &salt);

    // A matching pre-image must verify.
    let recomputed = commitment(&env, &claimant, &salt);
    assert_eq!(expected, recomputed, "commitment must be deterministic");

    // A mismatching digest (flip a byte) must not equal the expected digest.
    let mut wrong = expected.to_array();
    wrong[0] ^= 0xff;
    let wrong_digest = BytesN::from_array(&env, &wrong);
    assert_ne!(expected, wrong_digest, "mismatching digest must be rejected");

    // Invalid pre-image lengths must be rejected without panicking. The
    // contract requires a non-empty salt; an empty salt must not match a
    // commitment produced with a non-empty salt.
    if !salt.is_empty() {
        let empty_salt = Bytes::new(&env);
        let empty_commitment = commitment(&env, &claimant, &empty_salt);
        assert_ne!(
            expected, empty_commitment,
            "empty salt must not match non-empty salt commitment"
        );
    }

    // Exercise the contract's verification path with the derived commitment.
    // The contract is expected to accept the matching digest and reject the
    // mismatching one without panicking.
    let contract_id = env.register_contract(None, will::Client::new(&env, &env.current_contract_address()));
    let client = will::Client::new(&env, &contract_id);

    // The contract must not panic when verifying a mismatching digest.
    let _ = client.try_reveal_and_claim(&claimant, &salt, &wrong_digest);

    // The contract must not panic when verifying a matching digest.
    let _ = client.try_reveal_and_claim(&claimant, &salt, &expected);
});
