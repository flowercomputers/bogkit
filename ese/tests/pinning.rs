#[path = "../build_support.rs"]
mod build_support;

use std::fs;

#[test]
fn hash_verification_rejects_changed_artifact() {
    let path = std::env::temp_dir().join(format!("ese-hash-test-{}", std::process::id()));
    fs::write(&path, b"expected artifact").unwrap();
    let expected = "668e115bb849c00905916e2ae25533fc04d1c5896da3a8f426414448b4042af0";
    build_support::verify_sha256(&path, expected).unwrap();

    fs::write(&path, b"changed artifact").unwrap();
    let error = build_support::verify_sha256(&path, expected).unwrap_err();
    assert!(error.contains("SHA-256 mismatch"));
    assert!(error.contains(expected));
    fs::remove_file(path).unwrap();
}

#[test]
fn encoder_identity_covers_compatibility_inputs() {
    let identity = ese::ENCODER_IDENTITY;
    assert_eq!(identity.dimensions, ese::DIMENSIONS);
    assert!(ese::ENCODER_ID.contains(identity.model_revision));
    assert!(ese::ENCODER_ID.contains(identity.model_sha256));
    assert!(ese::ENCODER_ID.contains(identity.tokenizer_sha256));
    assert!(ese::ENCODER_ID.contains(identity.preprocessing_version));
    assert!(ese::ENCODER_ID.contains(&format!("dim-{}", identity.dimensions)));
    assert!(ese::ENCODER_ID.contains(&format!("scalar-{}", identity.scalar_type)));
}
