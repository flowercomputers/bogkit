use std::io::Cursor;

use freight_clearing::{Manifest, Proposal, clear_batch, validate_orders, validate_proposal};

#[test]
fn batch_output_is_canonical_and_checked_against_manifest_value() {
    // Catches map/input-order leakage, unchecked gross value, and accepting a
    // result that was not independently validated before publication.
    let input = concat!(
        "{\"order_id\":\"z-sell\",\"lane_id\":\"z-lane\",\"service_date\":\"2026-09-02\",\"side\":\"sell\",\"submitted_sequence\":1,\"quantity\":4,\"limit_price_cents\":200}\n",
        "{\"order_id\":\"a-buy\",\"lane_id\":\"a-lane\",\"service_date\":\"2026-09-01\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":100}\n",
        "{\"order_id\":\"z-buy\",\"lane_id\":\"z-lane\",\"service_date\":\"2026-09-02\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":4,\"limit_price_cents\":190}\n",
        "{\"order_id\":\"a-sell\",\"lane_id\":\"a-lane\",\"service_date\":\"2026-09-01\",\"side\":\"sell\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":90}\n",
    );
    let manifest = Manifest {
        order_count: 4,
        buy_count: 2,
        sell_count: 2,
        market_count: 2,
        max_fill_count: 2,
        expected_gross_value_cents: Some(270),
        orders_sha256: None,
    };
    let batch = validate_orders(Cursor::new(input.as_bytes()), &manifest).expect("valid batch");

    let proposal = clear_batch(&batch, &manifest).expect("valid proposal");
    validate_proposal(&batch, &proposal, &manifest).expect("validated proposal");

    assert_eq!(proposal.fills.len(), 1);
    assert_eq!(proposal.fills[0].lane_id, "a-lane");
    assert_eq!(proposal.gross_value_cents, 270);
}

#[test]
fn hand_written_over_cap_result_is_rejected() {
    // Catches treating the mathematical fill bound as documentation instead
    // of enforcing it against a loaded or generated result.
    let input = concat!(
        "{\"order_id\":\"b\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":100}\n",
        "{\"order_id\":\"s\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"sell\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":90}\n",
    );
    let manifest = Manifest {
        order_count: 2,
        buy_count: 1,
        sell_count: 1,
        market_count: 1,
        max_fill_count: 1,
        expected_gross_value_cents: None,
        orders_sha256: None,
    };
    let batch = validate_orders(Cursor::new(input.as_bytes()), &manifest).expect("valid batch");
    let valid = clear_batch(&batch, &manifest).expect("valid proposal");
    let proposal = Proposal {
        fills: vec![valid.fills[0].clone(), valid.fills[0].clone()],
        gross_value_cents: 540,
    };

    let error = validate_proposal(&batch, &proposal, &manifest).expect_err("over cap must fail");

    assert!(error.contains("maximum fill count"), "{error}");
}
