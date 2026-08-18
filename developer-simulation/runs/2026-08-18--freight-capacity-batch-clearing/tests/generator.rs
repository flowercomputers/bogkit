use std::fs;

use freight_clearing::{FailurePoint, FixtureSpec, generate_fixture, run_files};

#[test]
fn fixed_seed_generator_produces_exact_counts_and_identical_bytes() {
    // Catches off-by-one market/side counts or hidden nondeterminism in the
    // benchmark fixture generator.
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let spec = FixtureSpec {
        market_count: 5,
        buy_count: 40,
        sell_count: 25,
        seed: 0x0818,
    };

    let first_fixture = generate_fixture(first.path(), spec).expect("first fixture");
    let second_fixture = generate_fixture(second.path(), spec).expect("second fixture");

    assert_eq!(first_fixture.manifest.order_count, 65);
    assert_eq!(first_fixture.manifest.market_count, 5);
    assert_eq!(first_fixture.manifest.max_fill_count, 60);
    assert_eq!(first_fixture.manifest.buy_count, 40);
    assert_eq!(first_fixture.manifest.sell_count, 25);
    assert_eq!(
        fs::read(first_fixture.paths.orders).unwrap(),
        fs::read(second_fixture.paths.orders).unwrap()
    );
}

#[test]
fn generator_seed_shuffles_one_stable_snapshot_without_changing_proposal() {
    // Catches a supposed shuffle seed that silently changes order contents,
    // which would make the ten-seed determinism evidence meaningless.
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let base = FixtureSpec {
        market_count: 5,
        buy_count: 40,
        sell_count: 25,
        seed: 1,
    };
    let first_fixture = generate_fixture(first.path(), base).expect("seed one fixture");
    let second_fixture =
        generate_fixture(second.path(), FixtureSpec { seed: 2, ..base }).expect("seed two fixture");

    let first_result = run_files(&first_fixture.paths, FailurePoint::None).expect("first clear");
    let second_result = run_files(&second_fixture.paths, FailurePoint::None).expect("second clear");

    assert_ne!(
        fs::read(first_fixture.paths.orders).unwrap(),
        fs::read(second_fixture.paths.orders).unwrap()
    );
    assert_eq!(first_result.proposal_sha256, second_result.proposal_sha256);
}
