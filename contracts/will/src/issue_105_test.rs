#![cfg(test)]

//! Regression tests for issue #105: cancel a will directly from `Triggered`
//! status without first running `emergency_checkin`.
//!
//! Before the fix, `cancel_will` only accepted `Active` and
//! `PendingConfirmation`, forcing an owner whose will had already been
//! triggered to submit `emergency_checkin` and then `cancel_will` in two
//! separate transactions. The fix permits cancellation directly from
//! `Triggered` and unindexes the will from `TriggeredWills`.

use soroban_sdk::{
    testutils::{storage::Persistent as _, Address as _, Ledger},
    token::StellarAssetClient,
    vec, Address, Env,
};

use crate::storage;
use crate::{Allocation, Beneficiary, WillContract, WillContractClient, WillError, WillStatus};

const DAY: u64 = 86_400;

fn setup(env: &Env) -> (WillContractClient<'_>, Address, Address) {
    let owner = Address::generate(env);
    let sac = env.register_stellar_asset_contract_v2(owner.clone());
    let token_address = sac.address();
    StellarAssetClient::new(env, &token_address).mint(&owner, &10_000_000);

    let contract_id = env.register(WillContract, ());
    let client = WillContractClient::new(env, &contract_id);
    (client, owner, token_address)
}

fn create_will(
    env: &Env,
    client: &WillContractClient<'_>,
    owner: &Address,
    token_address: &Address,
    beneficiary: &Address,
) -> u64 {
    client.create_will(
        owner,
        &vec![env, (token_address.clone(), 1_000_000_i128)],
        &vec![
            env,
            Beneficiary {
                address: beneficiary.clone(),
                allocation: Allocation::Percentage(10_000),
            },
        ],
        &90,
        &7,
        &vec![env],
        &0,
        &None,
        &0,
    )
}

/// Drives a will into `Triggered` by advancing past the check-in period and
/// invoking `trigger_will` as a non-owner caller.
fn trigger_will(env: &Env, client: &WillContractClient<'_>, will_id: u64) {
    env.ledger().with_mut(|l| l.timestamp += 91 * DAY);
    client.trigger_will(&will_id);
}

#[test]
fn cancel_will_allows_cancellation_from_triggered_status() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_700_000_000);

    let owner = Address::generate(&env);
    let heir = Address::generate(&env);
    let (client, owner, token_address) = setup(&env);
    let will_id = create_will(&env, &client, &owner, &token_address, &heir);

    // Drive the will into `Triggered`.
    trigger_will(&env, &client, will_id);
    assert_eq!(client.get_will(&will_id).status, WillStatus::Triggered);

    // The will appears in the global TriggeredWills index while triggered.
    assert!(storage::get_triggered_wills(&env).contains(&will_id));

    // Owner cancels directly from `Triggered` — no emergency_checkin first.
    client.cancel_will(&will_id, &owner);

    let will = client.get_will(&will_id);
    assert_eq!(will.status, WillStatus::Cancelled);

    // The will is no longer indexed as triggered.
    assert!(!storage::get_triggered_wills(&env).contains(&will_id));
}

#[test]
fn cancel_will_from_triggered_transfers_funds_back_to_owner() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_700_000_000);

    let owner = Address::generate(&env);
    let heir = Address::generate(&env);
    let (client, owner, token_address) = setup(&env);
    let will_id = create_will(&env, &client, &owner, &token_address, &heir);

    trigger_will(&env, &client, will_id);

    let balance_before = StellarAssetClient::new(&env, &token_address).balance(&owner);
    client.cancel_will(&will_id, &owner);

    let balance_after = StellarAssetClient::new(&env, &token_address).balance(&owner);
    assert!(balance_after > balance_before, "owner must reclaim locked tokens");
    assert_eq!(client.get_will(&will_id).status, WillStatus::Cancelled);
}

#[test]
fn cancel_will_rejects_non_owner_from_triggered() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_700_000_000);

    let owner = Address::generate(&env);
    let stranger = Address::generate(&env);
    let heir = Address::generate(&env);
    let (client, owner, token_address) = setup(&env);
    let will_id = create_will(&env, &client, &owner, &token_address, &heir);

    trigger_will(&env, &client, will_id);

    let err = client
        .try_cancel_will(&will_id, &stranger)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, WillError::NotOwner.into());
    assert_eq!(client.get_will(&will_id).status, WillStatus::Triggered);
}
