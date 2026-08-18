use freight_clearing::{MarketKey, Order, Side, clear_market};

type GoldenCase = (u64, u64, u64, u64, Option<(u64, u64)>);

#[test]
fn forty_hand_checked_golden_markets() {
    // Each tuple is (buy quantity, sell quantity, buy limit, sell ask,
    // expected fill quantity and seller-ask price). Expectations are literal,
    // not computed with matcher helpers.
    let cases: [GoldenCase; 40] = [
        (1, 1, 100, 90, Some((1, 90))),
        (2, 1, 100, 90, Some((1, 90))),
        (1, 2, 100, 90, Some((1, 90))),
        (9, 4, 100, 100, Some((4, 100))),
        (4, 9, 100, 100, Some((4, 100))),
        (7, 7, 55, 54, Some((7, 54))),
        (7, 7, 55, 56, None),
        (1, 1, 1, 1, Some((1, 1))),
        (1, 1, 1, 2, None),
        (1_000_000, 1, 10_000_000, 1, Some((1, 1))),
        (1, 1_000_000, 10_000_000, 1, Some((1, 1))),
        (
            1_000_000,
            1_000_000,
            10_000_000,
            10_000_000,
            Some((1_000_000, 10_000_000)),
        ),
        (13, 8, 501, 500, Some((8, 500))),
        (8, 13, 501, 500, Some((8, 500))),
        (21, 21, 300, 299, Some((21, 299))),
        (34, 21, 300, 301, None),
        (55, 34, 700, 600, Some((34, 600))),
        (34, 55, 700, 600, Some((34, 600))),
        (89, 89, 42, 42, Some((89, 42))),
        (144, 89, 42, 43, None),
        (233, 144, 999, 1, Some((144, 1))),
        (144, 233, 999, 1, Some((144, 1))),
        (377, 377, 2, 2, Some((377, 2))),
        (610, 377, 2, 3, None),
        (987, 610, 5_000, 4_999, Some((610, 4_999))),
        (610, 987, 5_000, 4_999, Some((610, 4_999))),
        (1_597, 1_597, 73, 73, Some((1_597, 73))),
        (2_584, 1_597, 73, 74, None),
        (4_181, 2_584, 8_888, 7_777, Some((2_584, 7_777))),
        (2_584, 4_181, 8_888, 7_777, Some((2_584, 7_777))),
        (6_765, 6_765, 222, 111, Some((6_765, 111))),
        (10_946, 6_765, 222, 223, None),
        (17_711, 10_946, 91_001, 91_000, Some((10_946, 91_000))),
        (10_946, 17_711, 91_001, 91_000, Some((10_946, 91_000))),
        (28_657, 28_657, 6, 6, Some((28_657, 6))),
        (46_368, 28_657, 6, 7, None),
        (75_025, 46_368, 700_000, 699_999, Some((46_368, 699_999))),
        (46_368, 75_025, 700_000, 699_999, Some((46_368, 699_999))),
        (121_393, 121_393, 3_333, 3_333, Some((121_393, 3_333))),
        (196_418, 121_393, 3_333, 3_334, None),
    ];

    for (index, (buy_quantity, sell_quantity, buy_price, sell_price, expected)) in
        cases.into_iter().enumerate()
    {
        let lane_id = format!("golden-{index:02}");
        let key = MarketKey::new(&lane_id, "2026-09-01");
        let orders = [
            Order {
                order_id: format!("buy-{index:02}"),
                lane_id: lane_id.clone(),
                service_date: key.service_date.clone(),
                side: Side::Buy,
                submitted_sequence: 1,
                quantity: buy_quantity,
                limit_price_cents: buy_price,
            },
            Order {
                order_id: format!("sell-{index:02}"),
                lane_id: lane_id.clone(),
                service_date: key.service_date.clone(),
                side: Side::Sell,
                submitted_sequence: 1,
                quantity: sell_quantity,
                limit_price_cents: sell_price,
            },
        ];

        let fills = clear_market(&key, &orders).expect("valid golden market");
        match expected {
            Some((quantity, price)) => {
                assert_eq!(fills.len(), 1, "golden market {index}");
                assert_eq!(fills[0].quantity, quantity, "golden market {index}");
                assert_eq!(fills[0].price_cents, price, "golden market {index}");
            }
            None => assert!(fills.is_empty(), "golden market {index}"),
        }
    }
}
