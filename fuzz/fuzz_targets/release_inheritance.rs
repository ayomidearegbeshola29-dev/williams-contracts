//! Coverage-guided libFuzzer target for the estate release path.
//!
//! Drives `create_will` -> `trigger_will` -> `release_inheritance` (which
//! internally calls `distribute`) with arbitrary multi-token estates,
//! balances, bounties, and beneficiary permutations. After a successful
//! release the target asserts the token balance conservation invariant:
//! for every token, the sum of amounts paid to beneficiaries plus the
//! bounty plus any remainder retained by the contract equals the amount
//! that was locked at creation time.
//!
//! Run with:
//!
//! ```text
//! cargo +nightly fuzz run release_inheritance -- -runs=1000
//! ```

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env, Vec as SorobanVec,
};

use will::{Beneficiary, WillContract, WillContractClient};

/// Number of distinct addresses the fuzzer picks from.
const ADDRESS_POOL_SIZE: u8 = 6;

/// Ledger timestamp every scenario starts at.
const BASE_TIMESTAMP: u64 = 1_700_000_000;

/// Upper bound on how much of each test token the owner is funded with.
const MAX_MINT: i128 = 1_000_000_000_000_000_000;

/// Maximum number of tokens in a single fuzzed estate.
const MAX_TOKENS: usize = 4;

/// Maximum number of beneficiaries in a single fuzzed estate.
const MAX_BENEFICIARIES: usize = 8;

/// One beneficiary entry, addressed by pool slot rather than by `Address`.
#[derive(Clone, Debug, Arbitrary)]
struct BeneficiarySpec {
    address_slot: u8,
    basis_points: u32,
}

/// One token allocation in the estate.
#[derive(Clone, Debug, Arbitrary)]
struct TokenSpec {
    /// Amount minted to the owner before the call. Clamped to `0..=MAX_MINT`.
    mint: i128,
    /// Amount passed to `create_will` for this token, verbatim.
    amount: i128,
}

/// A full release scenario.
#[derive(Clone, Debug, Arbitrary)]
struct ReleaseInput {
    tokens: Vec<TokenSpec>,
    beneficiaries: Vec<BeneficiarySpec>,
    bounty: i128,
    checkin_period_days: u64,
    grace_period_days: u64,
}

fn clamp_mint(value: i128) -> i128 {
    value.rem_euclid(MAX_MINT)
}

fn build_addresses(env: &Env) -> [Address; ADDRESS_POOL_SIZE as usize] {
    let mut addresses: [Address; ADDRESS_POOL_SIZE as usize] = [
        Address::generate(env),
        Address::generate(env),
        Address::generate(env),
        Address::generate(env),
        Address::generate(env),
        Address::generate(env),
    ];
    // Deterministic ordering is not required; the pool is just a set.
    addresses.sort_by(|a, b| a.to_string().cmp(&b.to_string()));
    addresses
}

fuzz_target!(|input: ReleaseInput| {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(BASE_TIMESTAMP);

    let addresses = build_addresses(&env);
    let owner = addresses[0].clone();

    let contract_id = env.register(WillContract, ());
    let client = WillContractClient::new(&env, &contract_id);

    // Build the token set. Always at least one token so the release path is
    // actually exercised.
    let token_count = input.tokens.len().clamp(1, MAX_TOKENS);
    let mut tokens: Vec<Address> = Vec::new();
    let mut amounts: Vec<i128> = Vec::new();
    let mut locked: Vec<i128> = Vec::new();

    for i in 0..token_count {
        let spec = input
            .tokens
            .get(i)
            .cloned()
            .unwrap_or(TokenSpec { mint: 0, amount: 0 });
        let token_admin = Address::generate(&env);
        let token = env.register_stellar_asset_contract_v2(token_admin.clone());
        let token_address = token.address();
        let sac = StellarAssetClient::new(&env, &token_address);
        let mint = clamp_mint(spec.mint);
        if mint > 0 {
            sac.mint(&owner, &mint);
        }
        tokens.push(token_address);
        amounts.push(spec.amount);
        locked.push(spec.amount.max(0));
    }

    // Build the beneficiary list. Basis points are passed verbatim so the
    // contract's own validation is what rejects bad splits.
    let beneficiary_count = input.beneficiaries.len().clamp(1, MAX_BENEFICIARIES);
    let mut beneficiaries: SorobanVec<Beneficiary> = SorobanVec::new(&env);
    for i in 0..beneficiary_count {
        let spec = input
            .beneficiaries
            .get(i)
            .cloned()
            .unwrap_or(BeneficiarySpec {
                address_slot: 0,
                basis_points: 0,
            });
        let slot = (spec.address_slot as usize) % addresses.len();
        beneficiaries.push_back(Beneficiary {
            address: addresses[slot].clone(),
            basis_points: spec.basis_points,
        });
    }

    let checkin_period_days = input.checkin_period_days % 365;
    let grace_period_days = input.grace_period_days % 30;

    let create_result = client.try_create_will(
        &owner,
        &tokens,
        &amounts,
        &beneficiaries,
        &input.bounty,
        &checkin_period_days,
        &grace_period_days,
    );

    let will_id = match create_result {
        Ok(Ok(id)) => id,
        // Rejected with a declared error: nothing to release.
        Ok(Err(_)) | Err(_) => return,
    };

    // Trigger the will so it becomes releasable, then release it.
    let _ = client.try_trigger_will(&owner, &will_id);

    let release_result = client.try_release_inheritance(&owner, &will_id);
    if !matches!(release_result, Ok(Ok(_))) {
        // Release rejected (e.g. not yet triggered): nothing to check.
        return;
    }

    // Token balance conservation: for every token, the contract must hold
    // exactly the remainder that was not distributed. We verify that the
    // sum of what beneficiaries received plus the bounty plus the contract's
    // remaining balance equals the amount originally locked.
    for (i, token_address) in tokens.iter().enumerate() {
        let token_client = TokenClient::new(&env, &token_address);
        let contract_balance = token_client.balance(&contract_id);
        let original = locked[i];

        // The contract must never hold more than it locked, and must never
        // hold a negative balance (impossible for SAC, but assert anyway).
        assert!(
            contract_balance >= 0,
            "contract holds negative balance for token {i}"
        );
        assert!(
            contract_balance <= original,
            "contract holds more than locked for token {i}: {contract_balance} > {original}"
        );

        // Sum what each beneficiary actually received for this token.
        let mut distributed: i128 = 0;
        for beneficiary in beneficiaries.iter() {
            distributed = distributed.saturating_add(token_client.balance(&beneficiary.address));
        }

        // Conservation: distributed + bounty + contract remainder == locked.
        // The bounty is paid in the first token only by convention; account
        // for it on token 0.
        let bounty_for_token = if i == 0 { input.bounty.max(0) } else { 0 };
        let total = distributed
            .saturating_add(bounty_for_token)
            .saturating_add(contract_balance);
        assert!(
            total == original,
            "token {i} not conserved: distributed={distributed} bounty={bounty_for_token} remainder={contract_balance} locked={original}"
        );
    }
});
