use freight_clearing::{MarketKey, Order, Side, clear_market};

fn order(id: &str, side: Side, sequence: u64, quantity: u64, price: u64) -> Order {
    Order {
        order_id: id.to_owned(),
        lane_id: "sea-nyc".to_owned(),
        service_date: "2026-09-01".to_owned(),
        side,
        submitted_sequence: sequence,
        quantity,
        limit_price_cents: price,
    }
}

#[test]
fn seller_ask_and_partial_quantities_follow_the_specification() {
    // Catches using the buyer price, skipping the partially remaining buy,
    // or losing quantity when one buy spans multiple sells.
    let key = MarketKey::new("sea-nyc", "2026-09-01");
    let orders = vec![
        order("buy", Side::Buy, 1, 7, 120),
        order("sell-a", Side::Sell, 2, 3, 100),
        order("sell-b", Side::Sell, 3, 4, 110),
    ];

    let fills = clear_market(&key, &orders).expect("valid market");

    assert_eq!(fills.len(), 2);
    assert_eq!(fills[0].buy_id, "buy");
    assert_eq!(fills[0].sell_id, "sell-a");
    assert_eq!(fills[0].quantity, 3);
    assert_eq!(fills[0].price_cents, 100);
    assert_eq!(fills[1].sell_id, "sell-b");
    assert_eq!(fills[1].quantity, 4);
    assert_eq!(fills[1].price_cents, 110);
}

#[test]
fn equal_price_and_sequence_use_bytewise_order_id() {
    // Catches inheriting insertion order for the final priority tie-breaker.
    let key = MarketKey::new("sea-nyc", "2026-09-01");
    let orders = vec![
        order("buy-z", Side::Buy, 9, 1, 100),
        order("buy-a", Side::Buy, 9, 1, 100),
        order("sell", Side::Sell, 1, 1, 100),
    ];

    let fills = clear_market(&key, &orders).expect("valid market");

    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].buy_id, "buy-a");
}
