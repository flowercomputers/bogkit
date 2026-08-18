use std::{
    fs,
    io::{self, BufRead, Read},
    path::Path,
    process::Command,
};

use freight_clearing::{
    FailurePoint, FixtureSpec, Manifest, RunErrorState, RunPaths, generate_fixture, run_files,
    validate_orders,
};
use sha2::{Digest, Sha256};

fn small_fixture(directory: &Path) -> RunPaths {
    generate_fixture(
        directory,
        FixtureSpec {
            market_count: 1,
            buy_count: 1,
            sell_count: 1,
            seed: 7,
        },
    )
    .expect("generate fixture")
    .paths
}

fn digest(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).expect("read source")))
}

fn edit_manifest(
    paths: &RunPaths,
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) {
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&paths.manifest).unwrap()).unwrap();
    edit(value.as_object_mut().expect("manifest object"));
    fs::write(&paths.manifest, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

#[test]
fn proposal_equal_to_orders_is_rejected_without_changing_orders() {
    // Catches replacing the immutable order snapshot when output and input
    // are the same ordinary path.
    let directory = tempfile::tempdir().unwrap();
    let fixture = small_fixture(directory.path());
    let before = digest(&fixture.orders);
    let paths = RunPaths {
        proposal: fixture.orders.clone(),
        ..fixture
    };

    let error = run_files(&paths, FailurePoint::None).expect_err("orders alias must fail");

    assert!(error.contains("aliases an input"), "{error}");
    assert_eq!(digest(&paths.orders), before);
}

#[test]
fn resolved_proposal_equal_to_manifest_is_rejected_without_changing_manifest() {
    // Catches a lexical spelling difference bypassing the manifest alias
    // guard after path resolution.
    let directory = tempfile::tempdir().unwrap();
    let fixture = small_fixture(directory.path());
    let before = digest(&fixture.manifest);
    let paths = RunPaths {
        proposal: directory.path().join("subdir/../manifest.json"),
        ..fixture
    };
    fs::create_dir(directory.path().join("subdir")).unwrap();

    let error = run_files(&paths, FailurePoint::None).expect_err("manifest alias must fail");

    assert!(error.contains("aliases an input"), "{error}");
    assert_eq!(digest(&paths.manifest), before);
}

#[cfg(unix)]
#[test]
fn existing_hard_link_to_orders_is_rejected_by_file_identity() {
    // Catches comparing only canonical path strings when two existing names
    // refer to one inode.
    let directory = tempfile::tempdir().unwrap();
    let fixture = small_fixture(directory.path());
    let proposal = directory.path().join("proposal.ndjson");
    fs::hard_link(&fixture.orders, &proposal).unwrap();
    let before = digest(&fixture.orders);
    let paths = RunPaths {
        proposal,
        ..fixture
    };

    let error = run_files(&paths, FailurePoint::None).expect_err("hard-link alias must fail");

    assert!(error.contains("aliases an input"), "{error}");
    assert_eq!(digest(&paths.orders), before);
    assert_eq!(digest(&paths.proposal), before);
}

#[test]
fn relative_proposal_path_publishes_successfully() {
    // Catches treating Path::parent("") as an invalid directory after the
    // complete proposal has already been renamed into place.
    let directory = tempfile::tempdir().unwrap();
    small_fixture(directory.path());

    let output = Command::new(env!("CARGO_BIN_EXE_freight-clearing"))
        .current_dir(directory.path())
        .args(["clear", "orders.ndjson", "manifest.json", "proposal.ndjson"])
        .output()
        .expect("run clear CLI");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(directory.path().join("proposal.ndjson").is_file());
}

#[test]
fn post_rename_failure_reports_durability_uncertainty_not_nonpublication() {
    // Catches collapsing a directory-sync failure after rename into the same
    // error state as a failure that preserved the previous proposal.
    let directory = tempfile::tempdir().unwrap();
    let paths = small_fixture(directory.path());
    fs::write(&paths.proposal, b"previous\n").unwrap();

    let error = run_files(&paths, FailurePoint::AfterRenameBeforeDirectorySync)
        .expect_err("post-rename injection must report uncertainty");

    assert!(error.is_durability_uncertain(), "{error}");
    assert!(
        error
            .to_string()
            .starts_with("PUBLISHED_DURABILITY_UNCERTAIN:"),
        "{error}"
    );
    let published = fs::read_to_string(&paths.proposal).unwrap();
    assert!(published.contains("\"fill_index\":0"), "{published}");
    assert_ne!(published.as_bytes(), b"previous\n");
}

#[test]
fn pre_rename_failure_reports_not_published_and_preserves_previous_bytes() {
    // Catches accidentally labelling a modeled pre-rename failure as an
    // uncertain publication after adding the post-rename state.
    let directory = tempfile::tempdir().unwrap();
    let paths = small_fixture(directory.path());
    fs::write(&paths.proposal, b"previous\n").unwrap();

    let error =
        run_files(&paths, FailurePoint::AfterTempSync).expect_err("pre-rename injection must fail");

    assert_eq!(error.state(), RunErrorState::NotPublished);
    assert_eq!(fs::read(&paths.proposal).unwrap(), b"previous\n");
}

struct CountingBufRead {
    bytes: Vec<u8>,
    position: usize,
    consumed: usize,
}

impl Read for CountingBufRead {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let available = self.fill_buf()?;
        let count = available.len().min(output.len());
        output[..count].copy_from_slice(&available[..count]);
        self.consume(count);
        Ok(count)
    }
}

impl BufRead for CountingBufRead {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        Ok(&self.bytes[self.position..])
    }

    fn consume(&mut self, amount: usize) {
        self.position += amount;
        self.consumed += amount;
    }
}

#[test]
fn one_mib_overlong_line_is_rejected_without_consuming_beyond_the_bound() {
    // Catches read_until allocating and consuming the complete corrupt line
    // before applying the advertised 4 KiB limit.
    let mut bytes = vec![b'x'; 1024 * 1024];
    bytes.push(b'\n');
    let mut reader = CountingBufRead {
        bytes,
        position: 0,
        consumed: 0,
    };
    let manifest = Manifest {
        order_count: 0,
        buy_count: 0,
        sell_count: 0,
        market_count: 0,
        max_fill_count: 0,
        expected_gross_value_cents: None,
        orders_sha256: None,
    };

    let error = validate_orders(&mut reader, &manifest).expect_err("overlong line must fail");

    assert!(error.contains("line too long"), "{error}");
    assert!(
        reader.consumed <= 4_097,
        "consumed {} bytes before rejection",
        reader.consumed
    );
}

#[test]
fn run_boundary_rejects_missing_orders_digest_without_touching_orders() {
    // Catches treating a missing reference-integrity digest as permission to
    // publish evidence without checking the immutable snapshot.
    let directory = tempfile::tempdir().unwrap();
    let paths = small_fixture(directory.path());
    let orders_before = digest(&paths.orders);
    edit_manifest(&paths, |manifest| {
        manifest.remove("orders_sha256");
    });

    let error = run_files(&paths, FailurePoint::None).expect_err("missing digest must fail");

    assert!(error.contains("orders_sha256 is required"), "{error}");
    assert_eq!(digest(&paths.orders), orders_before);
    assert!(!paths.proposal.exists());
}

#[test]
fn run_boundary_rejects_null_orders_digest() {
    // Catches serde Option treating an explicit null integrity field as valid
    // evidence configuration.
    let directory = tempfile::tempdir().unwrap();
    let paths = small_fixture(directory.path());
    edit_manifest(&paths, |manifest| {
        manifest.insert("orders_sha256".to_owned(), serde_json::Value::Null);
    });

    let error = run_files(&paths, FailurePoint::None).expect_err("null digest must fail");

    assert!(error.contains("orders_sha256 is required"), "{error}");
    assert!(!paths.proposal.exists());
}

#[test]
fn run_boundary_rejects_malformed_orders_digest_before_comparison() {
    // Catches reporting a reference mismatch for a field that was not a
    // syntactically valid SHA-256 digest in the first place.
    for malformed in ["abc".to_owned(), "g".repeat(64)] {
        let directory = tempfile::tempdir().unwrap();
        let paths = small_fixture(directory.path());
        edit_manifest(&paths, |manifest| {
            manifest.insert(
                "orders_sha256".to_owned(),
                serde_json::Value::String(malformed),
            );
        });

        let error = run_files(&paths, FailurePoint::None).expect_err("malformed digest must fail");

        assert!(error.contains("valid 64-hex"), "{error}");
        assert!(!paths.proposal.exists());
    }
}

#[test]
fn run_boundary_rejects_missing_expected_gross_value() {
    // Catches publishing a proposal that was never compared with the
    // authoritative gross-value field at the evidence boundary.
    let directory = tempfile::tempdir().unwrap();
    let paths = small_fixture(directory.path());
    edit_manifest(&paths, |manifest| {
        manifest.remove("expected_gross_value_cents");
    });

    let error =
        run_files(&paths, FailurePoint::None).expect_err("missing expected gross must fail");

    assert!(
        error.contains("expected_gross_value_cents is required"),
        "{error}"
    );
    assert!(!paths.proposal.exists());
}

#[test]
fn run_boundary_rejects_null_expected_gross_value() {
    // Catches an explicit null bypassing the required authoritative value.
    let directory = tempfile::tempdir().unwrap();
    let paths = small_fixture(directory.path());
    edit_manifest(&paths, |manifest| {
        manifest.insert(
            "expected_gross_value_cents".to_owned(),
            serde_json::Value::Null,
        );
    });

    let error = run_files(&paths, FailurePoint::None).expect_err("null expected gross must fail");

    assert!(
        error.contains("expected_gross_value_cents is required"),
        "{error}"
    );
    assert!(!paths.proposal.exists());
}
