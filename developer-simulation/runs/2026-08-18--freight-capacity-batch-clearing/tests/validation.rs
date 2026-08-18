use std::io::Cursor;

use freight_clearing::{Manifest, validate_orders};

fn single_buy_manifest() -> Manifest {
    Manifest {
        order_count: 1,
        buy_count: 1,
        sell_count: 0,
        market_count: 1,
        max_fill_count: 0,
        expected_gross_value_cents: None,
        orders_sha256: None,
    }
}

#[test]
fn valid_complete_ndjson_is_grouped_only_after_manifest_counts_match() {
    // Catches trusting input arrival order or accepting a batch whose declared
    // counts were never checked.
    let input = concat!(
        "{\"order_id\":\"s1\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"sell\",\"submitted_sequence\":2,\"quantity\":3,\"limit_price_cents\":90}\n",
        "{\"order_id\":\"b1\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":100}\n",
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

    assert_eq!(batch.order_count(), 2);
    assert_eq!(batch.market_count(), 1);
}

#[test]
fn duplicate_global_order_id_rejects_the_whole_batch() {
    // Catches checking uniqueness only within one side or market.
    let input = concat!(
        "{\"order_id\":\"same\",\"lane_id\":\"lane-a\",\"service_date\":\"2026-09-01\",\"side\":\"sell\",\"submitted_sequence\":2,\"quantity\":3,\"limit_price_cents\":90}\n",
        "{\"order_id\":\"same\",\"lane_id\":\"lane-b\",\"service_date\":\"2026-09-02\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":100}\n",
    );
    let manifest = Manifest {
        order_count: 2,
        buy_count: 1,
        sell_count: 1,
        market_count: 2,
        max_fill_count: 0,
        expected_gross_value_cents: None,
        orders_sha256: None,
    };

    let error = validate_orders(Cursor::new(input.as_bytes()), &manifest)
        .expect_err("duplicate ID must fail");

    assert!(error.contains("duplicate order_id"), "{error}");
}

#[test]
fn record_without_final_newline_is_rejected_as_truncated() {
    // Catches accepting an NDJSON export that may have been cut mid-write.
    let input = "{\"order_id\":\"b1\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":100}";
    let manifest = Manifest {
        order_count: 1,
        buy_count: 1,
        sell_count: 0,
        market_count: 1,
        max_fill_count: 0,
        expected_gross_value_cents: None,
        orders_sha256: None,
    };

    let error = validate_orders(Cursor::new(input.as_bytes()), &manifest)
        .expect_err("truncated NDJSON must fail");

    assert!(error.contains("final newline"), "{error}");
}

#[test]
fn invalid_fields_are_rejected_before_any_market_is_cleared() {
    // Catches silently accepting a malformed snapshot row and deferring the
    // failure until after some market work has already happened.
    let cases = [
        (
            "{\"order_id\":\"\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":100}\n",
            "order_id",
        ),
        (
            "{\"order_id\":\"b1\",\"lane_id\":\"\",\"service_date\":\"2026-09-01\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":100}\n",
            "lane_id",
        ),
        (
            "{\"order_id\":\"bé\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":100}\n",
            "ASCII",
        ),
        (
            "{\"order_id\":\"b1\",\"lane_id\":\"lane\",\"service_date\":\"2026-02-30\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":100}\n",
            "service_date",
        ),
        (
            "{\"order_id\":\"b1\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"hold\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":100}\n",
            "unknown variant",
        ),
        (
            "{\"order_id\":\"b1\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"buy\",\"submitted_sequence\":-1,\"quantity\":3,\"limit_price_cents\":100}\n",
            "invalid value",
        ),
        (
            "{\"order_id\":\"b1\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":0,\"limit_price_cents\":100}\n",
            "quantity",
        ),
        (
            "{\"order_id\":\"b1\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":1000001,\"limit_price_cents\":100}\n",
            "quantity",
        ),
        (
            "{\"order_id\":\"b1\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":0}\n",
            "limit_price_cents",
        ),
        (
            "{\"order_id\":\"b1\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":10000001}\n",
            "limit_price_cents",
        ),
    ];

    for (input, expected) in cases {
        let error = validate_orders(Cursor::new(input.as_bytes()), &single_buy_manifest())
            .expect_err("invalid field must fail");
        assert!(
            error.contains(expected),
            "expected {expected:?} in {error:?}"
        );
    }
}

#[test]
fn invalid_utf8_is_rejected() {
    // Catches lossy decoding that could alter bytewise order IDs.
    let input = vec![0xff, b'\n'];
    let error = validate_orders(Cursor::new(input), &single_buy_manifest())
        .expect_err("invalid UTF-8 must fail");
    assert!(error.contains("invalid UTF-8"), "{error}");
}

#[test]
fn overlong_line_is_rejected_before_json_parsing() {
    // Catches unbounded line buffering on a corrupt export.
    let input = format!(
        "{{\"order_id\":\"b1\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":100,\"padding\":\"{}\"}}\n",
        "x".repeat(5_000)
    );
    let error = validate_orders(Cursor::new(input.as_bytes()), &single_buy_manifest())
        .expect_err("overlong line must fail");
    assert!(error.contains("line too long"), "{error}");
}
