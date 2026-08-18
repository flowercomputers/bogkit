use std::fmt::Write as _;
use std::fs;

use lab_unit_gate::{FailPoint, InputPaths, PublicationState, run};
use tempfile::tempdir;

fn write_inputs(root: &std::path::Path, observations: &str) {
    fs::write(
        root.join("units.ndjson"),
        "{\"symbol\":\"mg/dL\"}\n{\"symbol\":\"mmol/L\"}\n{\"symbol\":\"huge\"}\n",
    )
    .unwrap();
    fs::write(
        root.join("analytes.ndjson"),
        "{\"analyte_code\":\"GLU\",\"dimension\":\"mass_concentration\",\"target_scale\":1}\n",
    )
    .unwrap();
    fs::write(
        root.join("mappings.ndjson"),
        concat!(
            "{\"source_system\":\"gateway-a\",\"analyte_code\":\"GLU\",\"source_unit\":\"mg/dL\",\"target_unit\":\"mmol/L\",\"expected_dimension\":\"mass_concentration\",\"multiplier_num\":1,\"multiplier_den\":2,\"offset_num\":0,\"offset_den\":1}\n",
            "{\"source_system\":\"gateway-a\",\"analyte_code\":\"GLU\",\"source_unit\":\"huge\",\"target_unit\":\"mmol/L\",\"expected_dimension\":\"mass_concentration\",\"multiplier_num\":1000000000000000000,\"multiplier_den\":1,\"offset_num\":0,\"offset_den\":1}\n",
        ),
    )
    .unwrap();
    fs::write(root.join("observations.ndjson"), observations).unwrap();
}

fn paths(root: &std::path::Path) -> InputPaths {
    InputPaths {
        units: root.join("units.ndjson"),
        analytes: root.join("analytes.ndjson"),
        mappings: root.join("mappings.ndjson"),
        observations: root.join("observations.ndjson"),
    }
}

#[test]
fn complete_run_sorts_rows_and_rejects_unsafe_records() {
    let dir = tempdir().unwrap();
    write_inputs(
        dir.path(),
        concat!(
            "{\"observation_id\":\"z\",\"source_system\":\"gateway-a\",\"analyte_code\":\"GLU\",\"source_unit\":\"mg/dL\",\"declared_dimension\":\"mass_concentration\",\"value\":\"1\"}\n",
            "{\"observation_id\":\"a\",\"source_system\":\"gateway-a\",\"analyte_code\":\"GLU\",\"source_unit\":\"bogus\",\"declared_dimension\":\"mass_concentration\",\"value\":\"1\"}\n",
            "{\"observation_id\":\"m\",\"source_system\":\"gateway-a\",\"analyte_code\":\"GLU\",\"source_unit\":\"mg/dL\",\"declared_dimension\":\"amount_concentration\",\"value\":\"1\"}\n",
            "{\"observation_id\":\"b\",\"source_system\":\"gateway-a\",\"analyte_code\":\"GLU\",\"source_unit\":\"mg/dL\",\"declared_dimension\":\"mass_concentration\",\"value\":\"NaN\"}\n",
            "{\"observation_id\":\"c\",\"source_system\":\"gateway-a\",\"analyte_code\":\"GLU\",\"source_unit\":\"huge\",\"declared_dimension\":\"mass_concentration\",\"value\":\"99999999999999999999999999999\"}\n",
        ),
    );
    let report = dir.path().join("report.ndjson");

    let summary = run(&paths(dir.path()), &report, FailPoint::None).expect("complete run");

    assert_eq!(summary.total, 5);
    assert_eq!(summary.converted, 1);
    assert_eq!(summary.rejected, 4);
    assert_eq!(
        fs::read_to_string(report).unwrap(),
        concat!(
            "{\"observation_id\":\"a\",\"reason\":\"UNKNOWN_MAPPING\",\"status\":\"rejected\"}\n",
            "{\"observation_id\":\"b\",\"reason\":\"INVALID_VALUE\",\"status\":\"rejected\"}\n",
            "{\"observation_id\":\"c\",\"reason\":\"ARITHMETIC_OVERFLOW\",\"status\":\"rejected\"}\n",
            "{\"observation_id\":\"m\",\"reason\":\"INCOMPATIBLE_DIMENSION\",\"status\":\"rejected\"}\n",
            "{\"observation_id\":\"z\",\"status\":\"normalized\",\"target_unit\":\"mmol/L\",\"value\":\"0.5\"}\n",
        )
    );
}

#[test]
fn run_level_failure_preserves_an_existing_complete_report() {
    let dir = tempdir().unwrap();
    let duplicate = "{\"observation_id\":\"same\",\"source_system\":\"gateway-a\",\"analyte_code\":\"GLU\",\"source_unit\":\"mg/dL\",\"declared_dimension\":\"mass_concentration\",\"value\":\"1\"}\n";
    write_inputs(dir.path(), &format!("{duplicate}{duplicate}"));
    let report = dir.path().join("report.ndjson");
    fs::write(&report, b"previous-complete-report\n").unwrap();

    let error = run(&paths(dir.path()), &report, FailPoint::None).unwrap_err();
    assert_eq!(error.publication_state, PublicationState::NotPublished);
    assert_eq!(fs::read(&report).unwrap(), b"previous-complete-report\n");
}

#[test]
fn injected_exits_never_publish_a_partial_report_and_rerun_recovers() {
    let dir = tempdir().unwrap();
    let mut valid = String::new();
    for index in 0..100 {
        writeln!(
            valid,
            "{{\"observation_id\":\"obs-{index:03}\",\"source_system\":\"gateway-a\",\"analyte_code\":\"GLU\",\"source_unit\":\"mg/dL\",\"declared_dimension\":\"mass_concentration\",\"value\":\"1\"}}"
        )
        .unwrap();
    }
    write_inputs(dir.path(), &valid);
    let report = dir.path().join("report.ndjson");
    fs::write(&report, b"previous-complete-report\n").unwrap();

    assert!(run(&paths(dir.path()), &report, FailPoint::AfterRows(1)).is_err());
    assert_eq!(fs::read(&report).unwrap(), b"previous-complete-report\n");

    assert!(run(&paths(dir.path()), &report, FailPoint::AfterRows(50)).is_err());
    assert_eq!(fs::read(&report).unwrap(), b"previous-complete-report\n");

    assert!(run(&paths(dir.path()), &report, FailPoint::BeforeRename).is_err());
    assert_eq!(fs::read(&report).unwrap(), b"previous-complete-report\n");

    let summary = run(&paths(dir.path()), &report, FailPoint::None).expect("clean rerun");
    assert_eq!((summary.converted, summary.rejected), (100, 0));
    assert!(!dir.path().join(".report.ndjson.temporary").exists());
}

#[test]
fn output_cannot_alias_observations_or_a_resolved_reference_path() {
    let dir = tempdir().unwrap();
    let observation = "{\"observation_id\":\"one\",\"source_system\":\"gateway-a\",\"analyte_code\":\"GLU\",\"source_unit\":\"mg/dL\",\"declared_dimension\":\"mass_concentration\",\"value\":\"1\"}\n";
    write_inputs(dir.path(), observation);
    let inputs = paths(dir.path());
    let observation_bytes = fs::read(&inputs.observations).unwrap();

    let observation_error = run(&inputs, &inputs.observations, FailPoint::None).unwrap_err();

    assert_eq!(observation_error.code, "OUTPUT_ALIASES_INPUT");
    assert_eq!(fs::read(&inputs.observations).unwrap(), observation_bytes);
    assert!(!dir.path().join(".observations.ndjson.temporary").exists());

    fs::create_dir(dir.path().join("nested")).unwrap();
    let resolved_units = dir.path().join("nested/../units.ndjson");
    let unit_bytes = fs::read(&inputs.units).unwrap();

    let reference_error = run(&inputs, &resolved_units, FailPoint::None).unwrap_err();

    assert_eq!(reference_error.code, "OUTPUT_ALIASES_INPUT");
    assert_eq!(fs::read(&inputs.units).unwrap(), unit_bytes);
}

#[test]
fn existing_output_identity_and_temporary_aliases_are_rejected() {
    let dir = tempdir().unwrap();
    let observation = "{\"observation_id\":\"one\",\"source_system\":\"gateway-a\",\"analyte_code\":\"GLU\",\"source_unit\":\"mg/dL\",\"declared_dimension\":\"mass_concentration\",\"value\":\"1\"}\n";
    write_inputs(dir.path(), observation);
    let inputs = paths(dir.path());
    let hard_link_output = dir.path().join("hard-link-report.ndjson");
    fs::hard_link(&inputs.mappings, &hard_link_output).unwrap();
    let mapping_bytes = fs::read(&inputs.mappings).unwrap();

    let identity_error = run(&inputs, &hard_link_output, FailPoint::None).unwrap_err();

    assert_eq!(identity_error.code, "OUTPUT_ALIASES_INPUT");
    assert_eq!(fs::read(&inputs.mappings).unwrap(), mapping_bytes);
    assert_eq!(fs::read(&hard_link_output).unwrap(), mapping_bytes);

    let output = dir.path().join("report.ndjson");
    let temporary_input = dir.path().join(".report.ndjson.temporary");
    fs::rename(&inputs.units, &temporary_input).unwrap();
    let adjacent_inputs = InputPaths {
        units: temporary_input.clone(),
        ..inputs
    };
    let temporary_bytes = fs::read(&temporary_input).unwrap();

    let temporary_error = run(&adjacent_inputs, &output, FailPoint::None).unwrap_err();

    assert_eq!(temporary_error.code, "OUTPUT_TEMPORARY_ALIASES_INPUT");
    assert_eq!(fs::read(&temporary_input).unwrap(), temporary_bytes);
    assert!(!output.exists());
}

#[test]
fn post_rename_uncertainty_is_distinct_from_prepublication_failure() {
    let dir = tempdir().unwrap();
    let observation = "{\"observation_id\":\"one\",\"source_system\":\"gateway-a\",\"analyte_code\":\"GLU\",\"source_unit\":\"mg/dL\",\"declared_dimension\":\"mass_concentration\",\"value\":\"1\"}\n";
    write_inputs(dir.path(), observation);
    let report = dir.path().join("report.ndjson");
    fs::write(&report, b"previous-complete-report\n").unwrap();

    let error = run(
        &paths(dir.path()),
        &report,
        FailPoint::AfterRenameBeforeDirectorySync,
    )
    .unwrap_err();

    assert_eq!(error.code, "POST_RENAME_DURABILITY_UNCERTAIN");
    assert_eq!(
        error.publication_state,
        PublicationState::PublishedDurabilityUncertain
    );
    assert_ne!(fs::read(&report).unwrap(), b"previous-complete-report\n");
    assert!(
        fs::read_to_string(&report)
            .unwrap()
            .contains("\"status\":\"normalized\"")
    );
}
