use std::fs;
use std::path::Path;

use lab_unit_gate::{FailPoint, InputPaths, run};
use tempfile::tempdir;

fn write_reference(root: &Path) {
    fs::write(root.join("units.ndjson"), "{\"symbol\":\"U0\"}\n").unwrap();
    fs::write(
        root.join("analytes.ndjson"),
        "{\"analyte_code\":\"A0\",\"dimension\":\"D0\",\"target_scale\":0}\n",
    )
    .unwrap();
    fs::write(
        root.join("mappings.ndjson"),
        "{\"source_system\":\"G0\",\"analyte_code\":\"A0\",\"source_unit\":\"U0\",\"target_unit\":\"U0\",\"expected_dimension\":\"D0\",\"multiplier_num\":1,\"multiplier_den\":1,\"offset_num\":0,\"offset_den\":1}\n",
    )
    .unwrap();
}

fn input_paths(root: &Path) -> InputPaths {
    InputPaths {
        units: root.join("units.ndjson"),
        analytes: root.join("analytes.ndjson"),
        mappings: root.join("mappings.ndjson"),
        observations: root.join("observations.ndjson"),
    }
}

#[test]
fn retained_unknown_field_fixture_has_one_exact_outcome() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/negative/unknown-field-observation.ndjson");
    let dir = tempdir().unwrap();
    write_reference(dir.path());
    fs::copy(&fixture, dir.path().join("observations.ndjson")).unwrap();
    let report = dir.path().join("report.ndjson");
    fs::write(&report, b"previous\n").unwrap();

    let error = run(&input_paths(dir.path()), &report, FailPoint::None).unwrap_err();

    assert_eq!(error.code, "INVALID_NDJSON");
    assert_eq!(fs::read(&report).unwrap(), b"previous\n");
}

#[test]
fn invalid_utf8_and_overlong_lines_are_run_level_failures() {
    for (bytes, expected) in [
        (vec![0xff, b'\n'], "INVALID_NDJSON"),
        (vec![b'x'; 1_025], "OVERLONG_LINE"),
    ] {
        let dir = tempdir().unwrap();
        write_reference(dir.path());
        fs::write(dir.path().join("observations.ndjson"), bytes).unwrap();
        let error = run(
            &input_paths(dir.path()),
            &dir.path().join("report.ndjson"),
            FailPoint::None,
        )
        .unwrap_err();
        assert_eq!(error.code, expected);
    }
}

#[test]
fn malformed_reference_variants_fail_before_observation_processing() {
    let valid_observation = "{\"observation_id\":\"x\",\"source_system\":\"G0\",\"analyte_code\":\"A0\",\"source_unit\":\"U0\",\"declared_dimension\":\"D0\",\"value\":\"1\"}\n";
    let cases = [
        (
            "units.ndjson",
            "{\"symbol\":\"U0\"}\n{\"symbol\":\"U0\"}\n",
            "INVALID_UNIT_SNAPSHOT",
        ),
        (
            "mappings.ndjson",
            "{\"source_system\":\"G0\",\"analyte_code\":\"A0\",\"source_unit\":\"U0\",\"target_unit\":\"MISSING\",\"expected_dimension\":\"D0\",\"multiplier_num\":1,\"multiplier_den\":1,\"offset_num\":0,\"offset_den\":1}\n",
            "UNKNOWN_UNIT_SYMBOL",
        ),
        (
            "mappings.ndjson",
            "{\"source_system\":\"G0\",\"analyte_code\":\"A0\",\"source_unit\":\"U0\",\"target_unit\":\"U0\",\"expected_dimension\":\"D0\",\"multiplier_num\":1,\"multiplier_den\":0,\"offset_num\":0,\"offset_den\":1}\n",
            "INVALID_DENOMINATOR_OR_MULTIPLIER",
        ),
        (
            "analytes.ndjson",
            "{\"analyte_code\":\"A0\",\"dimension\":\"D0\",\"target_scale\":10}\n",
            "INVALID_ANALYTE_SNAPSHOT",
        ),
        (
            "mappings.ndjson",
            "{\"source_system\":\"G0\",\"analyte_code\":\"A0\",\"source_unit\":\"U0\",\"target_unit\":\"U0\",\"expected_dimension\":\"WRONG\",\"multiplier_num\":1,\"multiplier_den\":1,\"offset_num\":0,\"offset_den\":1}\n",
            "INCONSISTENT_DIMENSION",
        ),
        (
            "mappings.ndjson",
            concat!(
                "{\"source_system\":\"G0\",\"analyte_code\":\"A0\",\"source_unit\":\"U0\",\"target_unit\":\"U0\",\"expected_dimension\":\"D0\",\"multiplier_num\":1,\"multiplier_den\":1,\"offset_num\":0,\"offset_den\":1}\n",
                "{\"source_system\":\"G0\",\"analyte_code\":\"A0\",\"source_unit\":\"U0\",\"target_unit\":\"U0\",\"expected_dimension\":\"D0\",\"multiplier_num\":1,\"multiplier_den\":1,\"offset_num\":0,\"offset_den\":1}\n",
            ),
            "DUPLICATE_MAPPING_KEY",
        ),
        (
            "mappings.ndjson",
            "{\"source_system\":\"G0\",\"analyte_code\":\"A0\",\"source_unit\":\"U0\",\"target_unit\":\"U0\",\"expected_dimension\":\"D0\",\"multiplier_num\":1000000000000000001,\"multiplier_den\":1,\"offset_num\":0,\"offset_den\":1}\n",
            "COEFFICIENT_OUT_OF_RANGE",
        ),
    ];
    for (file, replacement, expected) in cases {
        let dir = tempdir().unwrap();
        write_reference(dir.path());
        fs::write(dir.path().join(file), replacement).unwrap();
        fs::write(dir.path().join("observations.ndjson"), valid_observation).unwrap();
        let error = run(
            &input_paths(dir.path()),
            &dir.path().join("report.ndjson"),
            FailPoint::None,
        )
        .unwrap_err();
        assert_eq!(error.code, expected);
    }
}

#[test]
fn forced_read_and_write_failures_return_nonzero_equivalent_errors() {
    let missing = tempdir().unwrap();
    write_reference(missing.path());
    let prior = missing.path().join("report.ndjson");
    fs::write(&prior, b"previous\n").unwrap();
    let read_error = run(&input_paths(missing.path()), &prior, FailPoint::None).unwrap_err();
    assert_eq!(read_error.code, "READ_ERROR");
    assert_eq!(fs::read(&prior).unwrap(), b"previous\n");

    let blocked = tempdir().unwrap();
    write_reference(blocked.path());
    fs::write(
        blocked.path().join("observations.ndjson"),
        "{\"observation_id\":\"x\",\"source_system\":\"G0\",\"analyte_code\":\"A0\",\"source_unit\":\"U0\",\"declared_dimension\":\"D0\",\"value\":\"1\"}\n",
    )
    .unwrap();
    fs::write(blocked.path().join("not-a-directory"), b"sentinel").unwrap();
    let write_error = run(
        &input_paths(blocked.path()),
        &blocked.path().join("not-a-directory/report.ndjson"),
        FailPoint::None,
    )
    .unwrap_err();
    assert_eq!(write_error.code, "WRITE_ERROR");
    assert_eq!(
        fs::read(blocked.path().join("not-a-directory")).unwrap(),
        b"sentinel"
    );
}
