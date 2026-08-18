use std::fs;

use lab_unit_gate::{FailPoint, GenerationConfig, InputPaths, generate_fixture, run};
use tempfile::tempdir;

#[test]
fn fixed_seed_generator_writes_declared_counts_and_sha256_manifest() {
    let first = tempdir().unwrap();
    let second = tempdir().unwrap();
    let config = GenerationConfig {
        seed: 42,
        ordinary: 95,
        boundary: 3,
        rejected: 2,
    };

    let first_manifest = generate_fixture(first.path(), config).expect("first fixture");
    let second_manifest = generate_fixture(second.path(), config).expect("second fixture");

    assert_eq!(first_manifest.observations.total, 100);
    assert_eq!(first_manifest.observations.ordinary, 95);
    assert_eq!(first_manifest.observations.boundary, 3);
    assert_eq!(first_manifest.observations.rejected, 2);
    assert_eq!(first_manifest.reference.mappings, 12_000);
    assert_eq!(first_manifest.reference.units, 250);
    assert_eq!(first_manifest.reference.analytes, 300);
    assert_eq!(first_manifest.digests, second_manifest.digests);
    assert_eq!(
        fs::read(first.path().join("observations.ndjson")).unwrap(),
        fs::read(second.path().join("observations.ndjson")).unwrap()
    );
    assert_eq!(first_manifest.digests["observations.ndjson"].len(), 64);
    assert!(first.path().join("manifest.json").is_file());
}

#[test]
fn five_fixed_shuffles_and_three_repeats_produce_identical_reports() {
    let mut digests = Vec::new();
    for seed in [11, 22, 33, 44, 55, 11, 11, 11] {
        let dir = tempdir().unwrap();
        generate_fixture(
            dir.path(),
            GenerationConfig {
                seed,
                ordinary: 950,
                boundary: 25,
                rejected: 25,
            },
        )
        .unwrap();
        let inputs = InputPaths {
            units: dir.path().join("units.ndjson"),
            analytes: dir.path().join("analytes.ndjson"),
            mappings: dir.path().join("mappings.ndjson"),
            observations: dir.path().join("observations.ndjson"),
        };
        let summary = run(&inputs, &dir.path().join("report.ndjson"), FailPoint::None).unwrap();
        assert_eq!((summary.converted, summary.rejected), (975, 25));
        digests.push(summary.output_sha256);
    }
    assert!(digests.windows(2).all(|pair| pair[0] == pair[1]));
}
