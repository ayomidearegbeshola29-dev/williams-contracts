#![cfg(test)]

//! Regression tests for issue #108: `batch_retry_failed_payouts` retries up
//! to `BATCH_MAX` failed payouts in a single invocation, clearing successes
//! from storage and leaving failures recorded.

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::Client as TokenClient,
    vec, Address, Env,
};

use crate::{
    issue_18_test::{FailableToken, FailableTokenClient},
    storage, Allocation, Beneficiary, FailedPayoutRequest, WillContract,
    WillContractClient, WillError,
};

const DAY: u64 = 86_400;

fn advance(env: &Env, days: u64) {
    env.ledger().with_mut(|l| l.timestamp += days * DAY);
}

/// Creates a will funded with `amount` of a failable token, triggers it, and
/// releases so `distribute` records one failed payout of `amount` for
/// `beneficiary`. Returns `(will_id, token_id)`.
fn make_failed_payout<'a>(
    env: &Env,
    owner: &Address,
    beneficiary: &Address,
    amount: i128,
    client: &WillContractClient<'a>,
) -> (u64, Address) {
    let token_id = env.register(FailableToken, ());
    let token_client = FailableTokenClient::new(&env, &token_id);
    token_client.mint(owner, amount);
    token_client.set_fail_all(&true);

    let will_id = client.create_will(
        owner,
        &vec![env, (token_id.clone(), amount)],
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
    );

    advance(env, 91);
    client.trigger_will(&will_id);
    advance(env, 8);
    client.release_inheritance(&will_id, &None);
    token_client.set_fail_all(&false);

    (will_id, token_id)
}

#[test]
fn batch_retry_clears_all_recorded_failed_payouts() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_700_000_000);

    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let contract_id = env.register(WillContract, ());
    let client = WillContractClient::new(&env, &contract_id);

    const AMOUNT: i128 = 500_000;
    const COUNT: u32 = 5;

    let mut requests: Vec<FailedPayoutRequest> = Vec::new(&env);
    let mut tokens: Vec<Address> = Vec::new(&env);
    let mut will_ids: Vec<u64> = Vec::new(&env);

    let mut i: u32 = 0;
    while i < COUNT {
        let (will_id, token_id) = make_failed_payout(
            &env, &owner, &beneficiary, AMOUNT, &client,
        );
        // Precondition: each distribute recorded a failed payout.
        assert_eq!(
            env.as_contract(&contract_id, || storage::get_failed_payout(
                &env, will_id, &token_id, &beneficiary
            )),
            Some(AMOUNT),
            "precondition: distribute must record each failed payout"
        );
        requests.push_back(FailedPayoutRequest {
            will_id,
            token: token_id.clone(),
            recipient: beneficiary.clone(),
        });
        tokens.push_back(token_id);
        will_ids.push_back(will_id);
        i += 1;
    }

    let succeeded = client.batch_retry_failed_payouts(&requests);

    // All five retries succeeded.
    assert_eq!(succeeded.len(), COUNT, "every recorded payout should be retried");
    for request in requests.iter() {
        assert!(succeeded.iter().any(|s| {
            s.will_id == request.will_id
                && s.token == request.token
                && s.recipient == request.recipient
        }), "succeeded list must contain {request:?}");
    }

    // Every failed-payout record is cleared from storage.
    let mut i: u32 = 0;
    while i < COUNT {
        assert_eq!(
            env.as_contract(&contract_id, || storage::get_failed_payout(
                &env,
                will_ids.get(i).expect("will id in bounds"),
                &tokens.get(i).expect("token in bounds"),
                &beneficiary,
            )),
            None,
            "failed payout {i} must be cleared after successful retry"
        );
        i += 1;
    }

    // The beneficiary received every token payout.
    let mut i: u32 = 0;
    while i < COUNT {
        let token_client = TokenClient::new(&env, &tokens.get(i).expect("token in bounds"));
        assert_eq!(
            token_client.balance(&beneficiary),
            AMOUNT,
            "beneficiary must receive payout {i}"
        );
        i += 1;
    }
}

#[test]
fn batch_retry_skips_requests_without_a_recorded_payout() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_700_000_000);

    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let contract_id = env.register(WillContract, ());
    let client = WillContractClient::new(&env, &contract_id);

    let (will_id, token_id) = make_failed_payout(
        &env, &owner, &beneficiary, 500_000, &client,
    );

    // One real request plus one request for a tuple that was never recorded.
    let stranger = Address::generate(&env);
    let mut requests: Vec<FailedPayoutRequest> = Vec::new(&env);
    requests.push_back(FailedPayoutRequest {
        will_id,
        token: token_id.clone(),
        recipient: beneficiary.clone(),
    });
    requests.push_back(FailedPayoutRequest {
        will_id: will_id + 1,
        token: token_id.clone(),
        recipient: stranger,
    });

    let succeeded = client.batch_retry_failed_payouts(&requests);

    // Only the real request was retried.
    assert_eq!(succeeded.len(), 1);
    assert_eq!(
        env.as_contract(&contract_id, || storage::get_failed_payout(
            &env, will_id, &token_id, &beneficiary
        )),
        None,
        "recorded payout must be cleared"
    );
}

#[test]
fn batch_retry_rejects_oversized_batch() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_700_000_000);

    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let contract_id = env.register(WillContract, ());
    let client = WillContractClient::new(&env, &contract_id);

    let (will_id, token_id) = make_failed_payout(
        &env, &owner, &beneficiary, 500_000, &client,
    );

    let mut oversized: Vec<FailedPayoutRequest> = Vec::new(&env);
    for _ in 0..11 {
        oversized.push_back(FailedPayoutRequest {
            will_id,
            token: token_id.clone(),
            recipient: beneficiary.clone(),
        });
    }

    let err = client
        .try_batch_retry_failed_payouts(&oversized)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, WillError::BatchTooLarge.into());
}
