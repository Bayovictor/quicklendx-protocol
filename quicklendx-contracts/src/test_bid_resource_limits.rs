//! Regression tests for bid resource and rate limits (#2449).
//!
//! Covers:
//! - Per-invoice bid ceiling (`MAX_BIDS_PER_INVOICE`) enforcement
//! - Per-investor active-bid ceiling (`MAX_ACTIVE_BIDS_PER_INVESTOR`) enforcement
//! - Bid input validation (zero/negative amounts, expected_return <= bid_amount)
//! - State safety: rejected bids leave no partial state
//! - Configurable TTL via `BidStorage::get_bid_ttl_days`
//! - Per-invoice index maintenance (`add_bid_to_invoice` called by `place_bid`)
//! - Ranking and best-bid consistency after limit enforcement
//! - Recovery after limit rejection (new bids succeed once space is freed)
//! - Deterministic ranking and tiebreaking at the integration boundary
//! - Stale/repeated bid handling under limits
//! - Cancellation under limits
//! - Boundary testing (limit-1, limit, limit+1)
//! - Adversarial: maximum-size bid collection

#![cfg(test)]

use crate::bid::{BidStorage, INVESTOR_BID_LIMIT_DISABLED, MAX_BIDS_PER_INVOICE};
use crate::contract::{QuickLendXContract, QuickLendXContractClient};
use crate::errors::QuickLendXError;
use crate::types::InvoiceCategory;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, BytesN, Env, Vec as SorobanVec,
};

const SECONDS_PER_DAY: u64 = 86_400;
static mut NONCE_COUNTER: u32 = 0;

fn setup() -> (Env, QuickLendXContractClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();
    let _ = env.host().set_invocation_resource_limits(None);
    env.ledger().set_timestamp(1_700_000_000);

    let contract_id = env.register(QuickLendXContract, ());
    let client = QuickLendXContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize_admin(&admin);

    let business = Address::generate(&env);
    client.submit_kyc_application(&business, &soroban_sdk::Bytes::from_array(&env, &[0u8; 10]));
    client.verify_business(&admin, &business);

    (env, client, admin, business)
}

fn create_investor(env: &Env, client: &QuickLendXContractClient) -> Address {
    let investor = Address::generate(env);
    client.submit_investor_kyc(&investor, &soroban_sdk::Bytes::from_array(env, &[0u8; 10]));
    client.verify_investor(&investor, &1_000_000_000_000i128);
    investor
}

fn store_invoice(
    env: &Env,
    client: &QuickLendXContractClient,
    business: &Address,
    amount: i128,
) -> BytesN<32> {
    let nonce_byte = unsafe {
        NONCE_COUNTER = NONCE_COUNTER.wrapping_add(1);
        NONCE_COUNTER as u8
    };
    env.ledger().set_timestamp(env.ledger().timestamp() + 1);

    let currency = Address::generate(env);
    let due_date = env.ledger().timestamp() + 30 * SECONDS_PER_DAY;
    let mut nonce_bytes = [0u8; 32];
    nonce_bytes[0] = nonce_byte;
    let nonce = BytesN::from_array(env, &nonce_bytes);
    let tags = SorobanVec::new(env);
    let description = soroban_sdk::Bytes::from_array(env, b"test invoice");

    client.store_invoice(
        business,
        &amount,
        &currency,
        &due_date,
        &description,
        &InvoiceCategory::Services,
        &tags,
        &nonce,
    )
}

fn place_bid(
    env: &Env,
    client: &QuickLendXContractClient,
    investor: &Address,
    invoice_id: &BytesN<32>,
    bid_amount: i128,
    expected_return: i128,
    salt: u8,
) -> BytesN<32> {
    client.place_bid(
        investor,
        invoice_id,
        &bid_amount,
        &expected_return,
        &BytesN::from_array(env, &[salt; 32]),
    )
}

fn assert_place_bid_err(
    client: &QuickLendXContractClient,
    investor: &Address,
    invoice_id: &BytesN<32>,
    bid_amount: i128,
    expected_return: i128,
    salt: [u8; 32],
    expected: QuickLendXError,
) {
    let err = client
        .try_place_bid(
            investor,
            invoice_id,
            &bid_amount,
            &expected_return,
            &BytesN::from_array(&client.env, &salt),
        )
        .unwrap_err()
        .expect("contract error");
    assert_eq!(err, expected);
}

fn cancel_bid_via_storage(env: &Env, client: &QuickLendXContractClient, bid_id: &BytesN<32>) {
    env.as_contract(&client.address, || {
        BidStorage::cancel_bid(&env, bid_id).unwrap();
    });
}

fn get_active_bid_count(
    env: &Env,
    client: &QuickLendXContractClient,
    invoice_id: &BytesN<32>,
) -> u32 {
    env.as_contract(&client.address, || {
        BidStorage::get_active_bid_count(&env, invoice_id)
    })
}

fn get_active_investor_bid_count(
    env: &Env,
    client: &QuickLendXContractClient,
    investor: &Address,
) -> u32 {
    env.as_contract(&client.address, || {
        BidStorage::count_active_placed_bids_for_investor(&env, investor)
    })
}

// ===========================================================================
// 1. Per-invoice bid ceiling
// ===========================================================================

#[test]
fn test_per_invoice_ceiling_enforced() {
    let (env, client, admin, business) = setup();

    client.set_max_active_bids_per_investor(&admin, &INVESTOR_BID_LIMIT_DISABLED);

    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);

    for i in 0..MAX_BIDS_PER_INVOICE {
        let investor = create_investor(&env, &client);
        place_bid(
            &env,
            &client,
            &investor,
            &invoice_id,
            1_000 + i as i128,
            1_100 + i as i128,
            i as u8,
        );
    }

    let count = get_active_bid_count(&env, &client, &invoice_id);
    assert_eq!(count, MAX_BIDS_PER_INVOICE);

    let next_investor = create_investor(&env, &client);
    assert_place_bid_err(
        &client,
        &next_investor,
        &invoice_id,
        1_200,
        1_300,
        [0xFF; 32],
        QuickLendXError::MaxBidsPerInvoiceExceeded,
    );

    let count_after = get_active_bid_count(&env, &client, &invoice_id);
    assert_eq!(count_after, MAX_BIDS_PER_INVOICE);
}

#[test]
fn test_per_invoice_boundary_limit_minus_one() {
    let (env, client, admin, business) = setup();
    client.set_max_active_bids_per_investor(&admin, &INVESTOR_BID_LIMIT_DISABLED);

    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);

    for i in 0..(MAX_BIDS_PER_INVOICE - 1) {
        let investor = create_investor(&env, &client);
        place_bid(
            &env,
            &client,
            &investor,
            &invoice_id,
            1_000 + i as i128,
            1_100 + i as i128,
            i as u8,
        );
    }

    let extra_investor = create_investor(&env, &client);
    place_bid(
        &env,
        &client,
        &extra_investor,
        &invoice_id,
        2_000,
        2_100,
        0xFF,
    );

    let count = get_active_bid_count(&env, &client, &invoice_id);
    assert_eq!(count, MAX_BIDS_PER_INVOICE);
}

// ===========================================================================
// 2. Per-investor active-bid ceiling
// ===========================================================================

#[test]
fn test_per_investor_active_bid_limit_enforced() {
    let (env, client, admin, business) = setup();

    let investor = create_investor(&env, &client);

    for i in 0..20u32 {
        let inv_id = store_invoice(&env, &client, &business, 10_000_000 + i as i128);
        client.verify_invoice(&admin, &inv_id);
        place_bid(&env, &client, &investor, &inv_id, 1_000, 1_100, i as u8);
    }

    let active = get_active_investor_bid_count(&env, &client, &investor);
    assert_eq!(active, 20);

    let extra_invoice = store_invoice(&env, &client, &business, 20_000_000);
    client.verify_invoice(&admin, &extra_invoice);
    assert_place_bid_err(
        &client,
        &investor,
        &extra_invoice,
        1_000,
        1_100,
        [0xFF; 32],
        QuickLendXError::MaxActiveBidsPerInvestorExceeded,
    );
}

#[test]
fn test_investor_limit_disabled_allows_unlimited() {
    let (env, client, admin, business) = setup();
    client.set_max_active_bids_per_investor(&admin, &INVESTOR_BID_LIMIT_DISABLED);

    let investor = create_investor(&env, &client);

    for i in 0..30u32 {
        let inv_id = store_invoice(&env, &client, &business, 10_000_000 + i as i128);
        client.verify_invoice(&admin, &inv_id);
        place_bid(&env, &client, &investor, &inv_id, 1_000, 1_100, i as u8);
    }

    let active = get_active_investor_bid_count(&env, &client, &investor);
    assert_eq!(active, 30);
}

#[test]
fn test_custom_investor_limit_enforced() {
    let (env, client, admin, business) = setup();
    client.set_max_active_bids_per_investor(&admin, &5);

    let investor = create_investor(&env, &client);

    for i in 0..5u32 {
        let inv_id = store_invoice(&env, &client, &business, 10_000_000 + i as i128);
        client.verify_invoice(&admin, &inv_id);
        place_bid(&env, &client, &investor, &inv_id, 1_000, 1_100, i as u8);
    }

    let inv6 = store_invoice(&env, &client, &business, 20_000_000);
    client.verify_invoice(&admin, &inv6);
    assert_place_bid_err(
        &client,
        &investor,
        &inv6,
        1_000,
        1_100,
        [0xFF; 32],
        QuickLendXError::MaxActiveBidsPerInvestorExceeded,
    );
}

// ===========================================================================
// 3. Bid input validation
// ===========================================================================

#[test]
fn test_zero_bid_amount_rejected() {
    let (env, client, admin, business) = setup();
    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);
    let investor = create_investor(&env, &client);

    assert_place_bid_err(
        &client,
        &investor,
        &invoice_id,
        0,
        1_100,
        [1u8; 32],
        QuickLendXError::InvalidAmount,
    );
}

#[test]
fn test_negative_bid_amount_rejected() {
    let (env, client, admin, business) = setup();
    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);
    let investor = create_investor(&env, &client);

    assert_place_bid_err(
        &client,
        &investor,
        &invoice_id,
        -100,
        1_100,
        [1u8; 32],
        QuickLendXError::InvalidAmount,
    );
}

#[test]
fn test_expected_return_must_exceed_bid_amount() {
    let (env, client, admin, business) = setup();
    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);
    let investor = create_investor(&env, &client);

    assert_place_bid_err(
        &client,
        &investor,
        &invoice_id,
        5_000,
        5_000,
        [1u8; 32],
        QuickLendXError::InvalidAmount,
    );
    assert_place_bid_err(
        &client,
        &investor,
        &invoice_id,
        5_000,
        4_999,
        [2u8; 32],
        QuickLendXError::InvalidAmount,
    );
}

#[test]
fn test_valid_bid_amount_succeeds() {
    let (env, client, admin, business) = setup();
    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);
    let investor = create_investor(&env, &client);

    let bid_id = client.place_bid(
        &investor,
        &invoice_id,
        &5_000,
        &6_000,
        &BytesN::from_array(&env, &[1u8; 32]),
    );

    let bid = client.get_bid(&bid_id).unwrap();
    assert_eq!(bid.bid_amount, 5_000);
    assert_eq!(bid.expected_return, 6_000);
}

// ===========================================================================
// 4. State safety: rejected bids leave no partial state
// ===========================================================================

#[test]
fn test_rejected_bid_leaves_no_state() {
    let (env, client, admin, business) = setup();
    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);
    let investor = create_investor(&env, &client);

    let count_before = get_active_bid_count(&env, &client, &invoice_id);

    let _ = client.try_place_bid(
        &investor,
        &invoice_id,
        &0,
        &1_100,
        &BytesN::from_array(&env, &[1u8; 32]),
    );

    let count_after = get_active_bid_count(&env, &client, &invoice_id);
    assert_eq!(count_before, count_after);
}

#[test]
fn test_ceiling_rejected_bid_leaves_no_state() {
    let (env, client, admin, business) = setup();
    client.set_max_active_bids_per_investor(&admin, &INVESTOR_BID_LIMIT_DISABLED);

    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);

    for i in 0..MAX_BIDS_PER_INVOICE {
        let inv = create_investor(&env, &client);
        place_bid(
            &env,
            &client,
            &inv,
            &invoice_id,
            1_000 + i as i128,
            1_100 + i as i128,
            i as u8,
        );
    }

    let count_before = get_active_bid_count(&env, &client, &invoice_id);

    let overflow_investor = create_investor(&env, &client);
    assert_place_bid_err(
        &client,
        &overflow_investor,
        &invoice_id,
        10_000,
        11_000,
        [0xFF; 32],
        QuickLendXError::MaxBidsPerInvoiceExceeded,
    );

    let count_after = get_active_bid_count(&env, &client, &invoice_id);
    assert_eq!(count_before, count_after);
}

// ===========================================================================
// 5. Configurable TTL
// ===========================================================================

#[test]
fn test_bid_uses_configurable_ttl() {
    let (env, client, admin, business) = setup();
    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);
    let investor = create_investor(&env, &client);

    let bid_id = client.place_bid(
        &investor,
        &invoice_id,
        &5_000,
        &6_000,
        &BytesN::from_array(&env, &[1u8; 32]),
    );

    let bid = client.get_bid(&bid_id).unwrap();
    let expected_expiry = env.ledger().timestamp() + 7 * SECONDS_PER_DAY;
    assert_eq!(bid.expiration_timestamp, expected_expiry);
}

// ===========================================================================
// 6. Per-invoice index maintenance
// ===========================================================================

#[test]
fn test_place_bid_registers_in_per_invoice_index() {
    let (env, client, admin, business) = setup();
    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);
    let investor1 = create_investor(&env, &client);
    let investor2 = create_investor(&env, &client);

    let bid_id1 = place_bid(&env, &client, &investor1, &invoice_id, 5_000, 6_000, 1);
    let bid_id2 = place_bid(&env, &client, &investor2, &invoice_id, 4_000, 5_000, 2);

    let bids = client.get_bids_for_invoice(&invoice_id);
    assert_eq!(bids.len(), 2);

    let mut found1 = false;
    let mut found2 = false;
    for i in 0..bids.len() {
        let b = bids.get(i).unwrap();
        if b.bid_id == bid_id1 {
            found1 = true;
        }
        if b.bid_id == bid_id2 {
            found2 = true;
        }
    }
    assert!(found1);
    assert!(found2);
}

// ===========================================================================
// 7. Ranking and best-bid consistency
// ===========================================================================

#[test]
fn test_best_bid_matches_ranked_head() {
    let (env, client, admin, business) = setup();
    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);

    let inv1 = create_investor(&env, &client);
    let inv2 = create_investor(&env, &client);
    let inv3 = create_investor(&env, &client);

    place_bid(&env, &client, &inv1, &invoice_id, 5_000, 7_000, 1);
    place_bid(&env, &client, &inv2, &invoice_id, 5_000, 6_500, 2);
    place_bid(&env, &client, &inv3, &invoice_id, 5_000, 6_000, 3);

    let best = client.get_best_bid(&invoice_id).expect("best must exist");
    let ranked = client.get_ranked_bids(&invoice_id);

    assert_eq!(ranked.len(), 3);
    assert_eq!(best.bid_id, ranked.get(0).unwrap().bid_id);

    for i in 1..ranked.len() {
        let prev = ranked.get(i - 1).unwrap();
        let cur = ranked.get(i).unwrap();
        let prev_profit = prev.expected_return - prev.bid_amount;
        let cur_profit = cur.expected_return - cur.bid_amount;
        assert!(prev_profit >= cur_profit);
    }
}

// ===========================================================================
// 8. Recovery after limit rejection
// ===========================================================================

#[test]
fn test_recovery_after_cancel_frees_slot() {
    let (env, client, admin, business) = setup();
    client.set_max_active_bids_per_investor(&admin, &INVESTOR_BID_LIMIT_DISABLED);

    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);

    let mut bid_ids = SorobanVec::<BytesN<32>>::new(&env);
    for i in 0..MAX_BIDS_PER_INVOICE {
        let inv = create_investor(&env, &client);
        let bid_id = place_bid(
            &env,
            &client,
            &inv,
            &invoice_id,
            1_000 + i as i128,
            1_100 + i as i128,
            i as u8,
        );
        bid_ids.push_back(bid_id);
    }

    let overflow = create_investor(&env, &client);
    assert_place_bid_err(
        &client,
        &overflow,
        &invoice_id,
        10_000,
        11_000,
        [0xFF; 32],
        QuickLendXError::MaxBidsPerInvoiceExceeded,
    );

    cancel_bid_via_storage(&env, &client, &bid_ids.get(0).unwrap());

    let new_investor = create_investor(&env, &client);
    let result = client.try_place_bid(
        &new_investor,
        &invoice_id,
        &10_000,
        &11_000,
        &BytesN::from_array(&env, &[0xAA; 32]),
    );
    assert!(result.is_ok());
}

#[test]
fn test_recovery_after_investor_limit_cancel() {
    let (env, client, admin, business) = setup();
    client.set_max_active_bids_per_investor(&admin, &3);

    let investor = create_investor(&env, &client);
    let mut bid_ids = SorobanVec::<BytesN<32>>::new(&env);

    for i in 0..3u32 {
        let inv_id = store_invoice(&env, &client, &business, 10_000_000 + i as i128);
        client.verify_invoice(&admin, &inv_id);
        let bid_id = place_bid(&env, &client, &investor, &inv_id, 1_000, 1_100, i as u8);
        bid_ids.push_back(bid_id);
    }

    let inv4 = store_invoice(&env, &client, &business, 20_000_000);
    client.verify_invoice(&admin, &inv4);
    assert_place_bid_err(
        &client,
        &investor,
        &inv4,
        1_000,
        1_100,
        [0xFF; 32],
        QuickLendXError::MaxActiveBidsPerInvestorExceeded,
    );

    cancel_bid_via_storage(&env, &client, &bid_ids.get(0).unwrap());

    let inv5 = store_invoice(&env, &client, &business, 30_000_000);
    client.verify_invoice(&admin, &inv5);
    let result = client.try_place_bid(
        &investor,
        &inv5,
        &1_000,
        &1_100,
        &BytesN::from_array(&env, &[0xAA; 32]),
    );
    assert!(result.is_ok());
}

// ===========================================================================
// 9. Duplicate / repeated bid rejection
// ===========================================================================

#[test]
fn test_duplicate_bid_rejected() {
    let (env, client, admin, business) = setup();
    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);
    let investor = create_investor(&env, &client);

    let _ = client.place_bid(
        &investor,
        &invoice_id,
        &5_000,
        &6_000,
        &BytesN::from_array(&env, &[42u8; 32]),
    );

    assert_place_bid_err(
        &client,
        &investor,
        &invoice_id,
        5_000,
        6_000,
        [42u8; 32],
        QuickLendXError::DuplicateBid,
    );
}

#[test]
fn test_different_salt_succeeds() {
    let (env, client, admin, business) = setup();
    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);
    let investor = create_investor(&env, &client);

    let _ = client.place_bid(
        &investor,
        &invoice_id,
        &5_000,
        &6_000,
        &BytesN::from_array(&env, &[1u8; 32]),
    );
    let _ = client.place_bid(
        &investor,
        &invoice_id,
        &5_000,
        &6_000,
        &BytesN::from_array(&env, &[2u8; 32]),
    );

    let bids = client.get_bids_for_invoice(&invoice_id);
    assert_eq!(bids.len(), 2);
}

// ===========================================================================
// 10. Stale bid handling under limits
// ===========================================================================

#[test]
fn test_expired_bids_free_slots() {
    let (env, client, admin, business) = setup();
    client.set_max_active_bids_per_investor(&admin, &3);
    client.set_bid_ttl_days(&admin, &1);

    let investor = create_investor(&env, &client);
    for i in 0..3u32 {
        let inv_id = store_invoice(&env, &client, &business, 10_000_000 + i as i128);
        client.verify_invoice(&admin, &inv_id);
        place_bid(&env, &client, &investor, &inv_id, 1_000, 1_100, i as u8);
    }

    let active_before = get_active_investor_bid_count(&env, &client, &investor);
    assert_eq!(active_before, 3);

    let now = env.ledger().timestamp();
    env.ledger().set_timestamp(now + 2 * SECONDS_PER_DAY);

    let expired = env.as_contract(&client.address, || {
        BidStorage::refresh_investor_bids(&env, &investor)
    });
    assert!(expired > 0);

    let active = get_active_investor_bid_count(&env, &client, &investor);
    assert_eq!(active, 0);

    let inv_new = store_invoice(&env, &client, &business, 20_000_000);
    client.verify_invoice(&admin, &inv_new);
    let result = client.try_place_bid(
        &investor,
        &inv_new,
        &1_000,
        &1_100,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    assert!(result.is_ok());

    client.reset_bid_ttl_to_default(&admin);
}

// ===========================================================================
// 11. Multiple invoices independent limits
// ===========================================================================

#[test]
fn test_per_invoice_limits_are_independent() {
    let (env, client, admin, business) = setup();
    client.set_max_active_bids_per_investor(&admin, &INVESTOR_BID_LIMIT_DISABLED);

    let inv1 = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &inv1);
    let inv2 = store_invoice(&env, &client, &business, 20_000_000);
    client.verify_invoice(&admin, &inv2);

    for i in 0..MAX_BIDS_PER_INVOICE {
        let investor = create_investor(&env, &client);
        place_bid(
            &env,
            &client,
            &investor,
            &inv1,
            1_000 + i as i128,
            1_100 + i as i128,
            i as u8,
        );
    }

    let investor = create_investor(&env, &client);
    let result = client.try_place_bid(
        &investor,
        &inv2,
        &5_000,
        &6_000,
        &BytesN::from_array(&env, &[1u8; 32]),
    );
    assert!(result.is_ok());
}

// ===========================================================================
// 12. Ranking determinism
// ===========================================================================

#[test]
fn test_ranking_is_deterministic() {
    let (env, client, admin, business) = setup();
    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);

    let inv1 = create_investor(&env, &client);
    let inv2 = create_investor(&env, &client);
    place_bid(&env, &client, &inv1, &invoice_id, 5_000, 7_000, 1);
    place_bid(&env, &client, &inv2, &invoice_id, 4_000, 6_000, 2);

    let ranked1 = client.get_ranked_bids(&invoice_id);
    let ranked2 = client.get_ranked_bids(&invoice_id);

    assert_eq!(ranked1.len(), ranked2.len());
    for i in 0..ranked1.len() {
        assert_eq!(
            ranked1.get(i).unwrap().bid_id,
            ranked2.get(i).unwrap().bid_id
        );
    }
}

// ===========================================================================
// 13. Cancellation under limits
// ===========================================================================

#[test]
fn test_cancel_on_full_invoice_allows_new_bid() {
    let (env, client, admin, business) = setup();
    client.set_max_active_bids_per_investor(&admin, &INVESTOR_BID_LIMIT_DISABLED);

    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);

    let mut first_bid_id: Option<BytesN<32>> = None;
    for i in 0..MAX_BIDS_PER_INVOICE {
        let inv = create_investor(&env, &client);
        let bid_id = place_bid(
            &env,
            &client,
            &inv,
            &invoice_id,
            1_000 + i as i128,
            1_100 + i as i128,
            i as u8,
        );
        if i == 0 {
            first_bid_id = Some(bid_id);
        }
    }

    let bid_to_cancel = first_bid_id.unwrap();
    cancel_bid_via_storage(&env, &client, &bid_to_cancel);

    let cancelled_bid = client.get_bid(&bid_to_cancel).unwrap();
    assert_eq!(cancelled_bid.status, crate::types::BidStatus::Cancelled);

    let new_investor = create_investor(&env, &client);
    let result = client.try_place_bid(
        &new_investor,
        &invoice_id,
        &10_000,
        &11_000,
        &BytesN::from_array(&env, &[0xAA; 32]),
    );
    assert!(result.is_ok());
}

// ===========================================================================
// 14. Integration: bid rejection + ranking + best bid
// ===========================================================================

#[test]
fn test_ranking_with_mixed_statuses() {
    let (env, client, admin, business) = setup();
    client.set_max_active_bids_per_investor(&admin, &INVESTOR_BID_LIMIT_DISABLED);

    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);

    let inv1 = create_investor(&env, &client);
    let inv2 = create_investor(&env, &client);
    let inv3 = create_investor(&env, &client);
    let inv4 = create_investor(&env, &client);
    let inv5 = create_investor(&env, &client);

    let bid1 = place_bid(&env, &client, &inv1, &invoice_id, 5_000, 8_000, 1);
    let bid2 = place_bid(&env, &client, &inv2, &invoice_id, 5_000, 7_000, 2);
    let _bid3 = place_bid(&env, &client, &inv3, &invoice_id, 5_000, 6_000, 3);
    let _bid4 = place_bid(&env, &client, &inv4, &invoice_id, 5_000, 6_500, 4);
    let _bid5 = place_bid(&env, &client, &inv5, &invoice_id, 5_000, 5_500, 5);

    cancel_bid_via_storage(&env, &client, &bid1);

    let best = client.get_best_bid(&invoice_id).expect("best must exist");
    assert_eq!(best.bid_id, bid2);

    let ranked = client.get_ranked_bids(&invoice_id);
    assert_eq!(ranked.len(), 4);
    assert_eq!(ranked.get(0).unwrap().bid_id, bid2);
}

// ===========================================================================
// 15. Cleanup + ceiling interaction
// ===========================================================================

#[test]
fn test_cleanup_frees_ceiling_slots() {
    let (env, client, admin, business) = setup();
    client.set_max_active_bids_per_investor(&admin, &INVESTOR_BID_LIMIT_DISABLED);
    client.set_bid_ttl_days(&admin, &1);

    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);

    for i in 0..5u32 {
        let inv = create_investor(&env, &client);
        place_bid(
            &env,
            &client,
            &inv,
            &invoice_id,
            1_000 + i as i128,
            1_100 + i as i128,
            i as u8,
        );
    }

    let count_before = get_active_bid_count(&env, &client, &invoice_id);
    assert_eq!(count_before, 5);

    let now = env.ledger().timestamp();
    env.ledger().set_timestamp(now + 2 * SECONDS_PER_DAY);

    let cleaned = client.cleanup_expired_bids(&invoice_id);
    assert!(cleaned > 0);

    let count_after = get_active_bid_count(&env, &client, &invoice_id);
    assert!(count_after < 5);

    client.reset_bid_ttl_to_default(&admin);
}

// ===========================================================================
// 16. Both limits active simultaneously
// ===========================================================================

#[test]
fn test_both_limits_enforce_simultaneously() {
    let (env, client, admin, business) = setup();
    client.set_max_active_bids_per_investor(&admin, &3);

    let invoice_id = store_invoice(&env, &client, &business, 10_000_000);
    client.verify_invoice(&admin, &invoice_id);
    let investor = create_investor(&env, &client);

    for i in 0..3u32 {
        let inv_id = store_invoice(&env, &client, &business, 10_000_000 + i as i128);
        client.verify_invoice(&admin, &inv_id);
        place_bid(&env, &client, &investor, &inv_id, 1_000, 1_100, i as u8);
    }

    let inv4 = store_invoice(&env, &client, &business, 20_000_000);
    client.verify_invoice(&admin, &inv4);
    assert_place_bid_err(
        &client,
        &investor,
        &inv4,
        1_000,
        1_100,
        [0xFF; 32],
        QuickLendXError::MaxActiveBidsPerInvestorExceeded,
    );

    let inv_heavy = store_invoice(&env, &client, &business, 30_000_000);
    client.verify_invoice(&admin, &inv_heavy);
    for i in 0..MAX_BIDS_PER_INVOICE {
        let inv = create_investor(&env, &client);
        place_bid(
            &env,
            &client,
            &inv,
            &inv_heavy,
            1_000 + i as i128,
            1_100 + i as i128,
            i as u8,
        );
    }

    let overflow_inv = create_investor(&env, &client);
    assert_place_bid_err(
        &client,
        &overflow_inv,
        &inv_heavy,
        1_000,
        1_100,
        [0xFF; 32],
        QuickLendXError::MaxBidsPerInvoiceExceeded,
    );
}

// ===========================================================================
// 17. Error constants sanity
// ===========================================================================

#[test]
fn test_error_variants_distinct() {
    assert_ne!(
        QuickLendXError::MaxBidsPerInvoiceExceeded as u32,
        QuickLendXError::MaxActiveBidsPerInvestorExceeded as u32,
    );
    assert_ne!(
        QuickLendXError::MaxBidsPerInvoiceExceeded as u32,
        QuickLendXError::InvalidAmount as u32,
    );
}

// ===========================================================================
// 18. Constant sanity
// ===========================================================================

#[test]
fn test_bid_limits_constants_are_sane() {
    assert!(MAX_BIDS_PER_INVOICE > 0);
    assert!(MAX_BIDS_PER_INVOICE <= 100);
}
