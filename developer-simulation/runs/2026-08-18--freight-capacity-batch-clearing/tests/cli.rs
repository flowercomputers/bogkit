use std::{fs, process::Command};

use sha2::{Digest, Sha256};

#[test]
fn clear_command_publishes_and_prints_machine_readable_result() {
    // Catches a CLI that cannot exercise the tested library boundary or emits
    // only human prose for the harness.
    let directory = tempfile::tempdir().unwrap();
    let orders = directory.path().join("orders.ndjson");
    let manifest = directory.path().join("manifest.json");
    let proposal = directory.path().join("proposal.ndjson");
    let order_bytes = concat!(
        "{\"order_id\":\"b\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":100}\n",
        "{\"order_id\":\"s\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"sell\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":90}\n",
    )
    .as_bytes();
    fs::write(&orders, order_bytes).unwrap();
    let order_digest = format!("{:x}", Sha256::digest(order_bytes));
    fs::write(
        &manifest,
        format!(
            "{{\"order_count\":2,\"buy_count\":1,\"sell_count\":1,\"market_count\":1,\"max_fill_count\":1,\"expected_gross_value_cents\":270,\"orders_sha256\":\"{order_digest}\"}}\n"
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_freight-clearing"))
        .args([
            "clear",
            orders.to_str().unwrap(),
            manifest.to_str().unwrap(),
            proposal.to_str().unwrap(),
        ])
        .output()
        .expect("run CLI");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON stdout");
    assert_eq!(result["fill_count"], 1);
    assert!(proposal.exists());
}
