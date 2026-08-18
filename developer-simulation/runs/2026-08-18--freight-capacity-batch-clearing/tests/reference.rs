use freight_clearing::{MarketKey, Order, Side, clear_market, slow_reference_market};

fn order(id: &str, side: Side, sequence: u64, quantity: u64, price: u64) -> Order {
    Order {
        order_id: id.to_owned(),
        lane_id: "lax-chi".to_owned(),
        service_date: "2026-10-02".to_owned(),
        side,
        submitted_sequence: sequence,
        quantity,
        limit_price_cents: price,
    }
}

#[test]
fn optimized_matcher_matches_independent_scan_reference() {
    // Catches a sorting or cursor mutation that disagrees with an
    // independently structured, deliberately slow priority scan.
    let key = MarketKey::new("lax-chi", "2026-10-02");
    let orders = vec![
        order("b2", Side::Buy, 2, 7, 110),
        order("s2", Side::Sell, 4, 5, 105),
        order("b1", Side::Buy, 1, 3, 110),
        order("s1", Side::Sell, 3, 8, 100),
    ];

    let actual = clear_market(&key, &orders).expect("optimized matcher");
    let reference = slow_reference_market(&key, &orders).expect("reference matcher");

    assert_eq!(actual, reference);
}

#[test]
fn ten_thousand_fixed_seed_markets_match_the_slow_reference() {
    // Catches priority and remaining-quantity interactions outside the
    // hand-written examples. The seed is printed in every mismatch message.
    const SEED: u64 = 0x5eed_f012_2026_0818;
    let mut rng = Lcg(SEED);
    for market_number in 0..10_000_u64 {
        let key = MarketKey::new(&format!("lane-{market_number}"), "2026-10-02");
        let buy_count = 1 + rng.range(4);
        let sell_count = 1 + rng.range(4);
        let mut orders = Vec::new();
        for index in 0..buy_count {
            orders.push(Order {
                order_id: format!("b-{market_number}-{index}"),
                lane_id: key.lane_id.clone(),
                service_date: key.service_date.clone(),
                side: Side::Buy,
                submitted_sequence: rng.range(4),
                quantity: 1 + rng.range(10),
                limit_price_cents: 1 + rng.range(20),
            });
        }
        for index in 0..sell_count {
            orders.push(Order {
                order_id: format!("s-{market_number}-{index}"),
                lane_id: key.lane_id.clone(),
                service_date: key.service_date.clone(),
                side: Side::Sell,
                submitted_sequence: rng.range(4),
                quantity: 1 + rng.range(10),
                limit_price_cents: 1 + rng.range(20),
            });
        }
        for index in (1..orders.len()).rev() {
            let swap_with = usize::try_from(rng.range(u64::try_from(index + 1).unwrap())).unwrap();
            orders.swap(index, swap_with);
        }

        let actual = clear_market(&key, &orders).expect("optimized matcher");
        let reference = slow_reference_market(&key, &orders).expect("reference matcher");
        assert_eq!(
            actual, reference,
            "seed={SEED:#x}, market_number={market_number}"
        );
    }
}

struct Lcg(u64);

impl Lcg {
    fn range(&mut self, upper: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 % upper
    }
}
