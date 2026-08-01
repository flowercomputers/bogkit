use std::fs::{self, OpenOptions};
use std::io;
use std::path::Path;

use crate::archive::Writer;
use crate::preflight::{FileDeclaration, Inventory, Manifest};
use crate::sha256::{Sha256, hex};

const PROGRAM: &[u8] = b"%\nT01 M06\nG00 X0 Y0\nM30\n%\n";
const SECOND_PROGRAM: &[u8] = b"%\nT02 M06\nM30\n%\n";
const TWO_GIB: u32 = 2 * 1024 * 1024 * 1024;
const TWO_GIB_ZERO_SHA256: &str =
    "a7c744c13cc101ed66c29f672f92455547889cc586ce6d44fe76ae824958ea51";
const TWO_GIB_ZERO_CRC32: u32 = 0x4dbd_f21c;

#[allow(clippy::too_many_lines)]
pub fn generate(output: &Path, include_huge: bool) -> io::Result<()> {
    fs::create_dir_all(output)?;
    write_json(
        &output.join("tools.json"),
        &Inventory {
            tool_ids: vec!["T01".to_owned(), "T02".to_owned()],
        },
    )?;

    let valid = base_manifest();
    bundle(
        &output.join("valid.zip"),
        &valid,
        &[("programs/job.nc", PROGRAM)],
    )?;

    let mut mismatch = base_manifest();
    mismatch.files[0].sha256 = "0".repeat(64);
    bundle(
        &output.join("checksum-mismatch.zip"),
        &mismatch,
        &[("programs/job.nc", PROGRAM)],
    )?;

    bundle(
        &output.join("undeclared.zip"),
        &base_manifest(),
        &[
            ("programs/job.nc", PROGRAM),
            ("programs/extra.gcode", SECOND_PROGRAM),
        ],
    )?;

    let mut missing = base_manifest();
    missing
        .files
        .push(declaration("programs/missing.nc", SECOND_PROGRAM));
    bundle(
        &output.join("missing.zip"),
        &missing,
        &[("programs/job.nc", PROGRAM)],
    )?;

    let duplicate_case = Manifest {
        version: 1,
        files: vec![
            declaration("programs/job.nc", PROGRAM),
            declaration("PROGRAMS/JOB.NC", SECOND_PROGRAM),
        ],
        entry_program: "programs/job.nc".to_owned(),
        required_tool_ids: vec!["T01".to_owned()],
    };
    bundle(
        &output.join("duplicate-case.zip"),
        &duplicate_case,
        &[
            ("programs/job.nc", PROGRAM),
            ("PROGRAMS/JOB.NC", SECOND_PROGRAM),
        ],
    )?;

    let absolute = Manifest {
        version: 1,
        files: vec![declaration("/escape.nc", PROGRAM)],
        entry_program: "/escape.nc".to_owned(),
        required_tool_ids: vec!["T01".to_owned()],
    };
    bundle(
        &output.join("absolute-path.zip"),
        &absolute,
        &[("/escape.nc", PROGRAM)],
    )?;

    let traversal = Manifest {
        version: 1,
        files: vec![declaration("../escape.nc", PROGRAM)],
        entry_program: "../escape.nc".to_owned(),
        required_tool_ids: vec!["T01".to_owned()],
    };
    bundle(
        &output.join("parent-traversal.zip"),
        &traversal,
        &[("../escape.nc", PROGRAM)],
    )?;

    let mut missing_tool = base_manifest();
    missing_tool.required_tool_ids = vec!["T404".to_owned()];
    bundle(
        &output.join("missing-tool.zip"),
        &missing_tool,
        &[("programs/job.nc", PROGRAM)],
    )?;

    let multi_error = Manifest {
        version: 2,
        files: vec![
            FileDeclaration {
                path: "programs/job.nc".to_owned(),
                bytes: 999,
                sha256: "0".repeat(64),
            },
            declaration("programs/missing.nc", SECOND_PROGRAM),
            FileDeclaration {
                path: "notes.txt".to_owned(),
                bytes: 1,
                sha256: "invalid".to_owned(),
            },
            declaration("programs/job.nc", PROGRAM),
        ],
        entry_program: "programs/not-declared.nc".to_owned(),
        required_tool_ids: vec!["T404".to_owned(), "T404".to_owned(), String::new()],
    };
    bundle(
        &output.join("multi-error.zip"),
        &multi_error,
        &[
            ("programs/job.nc", PROGRAM),
            ("programs/extra.gcode", SECOND_PROGRAM),
        ],
    )?;

    bundle(
        &output.join("truncated.zip"),
        &base_manifest(),
        &[("programs/job.nc", PROGRAM)],
    )?;
    let truncated = OpenOptions::new()
        .write(true)
        .open(output.join("truncated.zip"))?;
    let shortened = truncated.metadata()?.len().saturating_sub(11);
    truncated.set_len(shortened)?;

    thousand_file_bundle(&output.join("thousand-files.zip"))?;

    if include_huge {
        oversized_bundle(&output.join("oversized-2gib-sparse.zip"))?;
    }
    Ok(())
}

fn base_manifest() -> Manifest {
    Manifest {
        version: 1,
        files: vec![declaration("programs/job.nc", PROGRAM)],
        entry_program: "programs/job.nc".to_owned(),
        required_tool_ids: vec!["T01".to_owned()],
    }
}

fn declaration(path: &str, data: &[u8]) -> FileDeclaration {
    FileDeclaration {
        path: path.to_owned(),
        bytes: data.len() as u64,
        sha256: digest(data),
    }
}

fn digest(data: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(data);
    hex(&hash.finish())
}

fn bundle(path: &Path, manifest: &Manifest, members: &[(&str, &[u8])]) -> io::Result<()> {
    let manifest = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
    let mut writer = Writer::create(path)?;
    writer.member("manifest.json", &manifest)?;
    for (name, data) in members {
        writer.member(name, data)?;
    }
    writer.finish()
}

fn oversized_bundle(path: &Path) -> io::Result<()> {
    let manifest = Manifest {
        version: 1,
        files: vec![FileDeclaration {
            path: "programs/huge.nc".to_owned(),
            bytes: u64::from(TWO_GIB),
            sha256: TWO_GIB_ZERO_SHA256.to_owned(),
        }],
        entry_program: "programs/huge.nc".to_owned(),
        required_tool_ids: vec!["T01".to_owned()],
    };
    let manifest = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
    let mut writer = Writer::create(path)?;
    writer.member("manifest.json", &manifest)?;
    writer.sparse_zero_member("programs/huge.nc", TWO_GIB, TWO_GIB_ZERO_CRC32)?;
    writer.finish()
}

fn thousand_file_bundle(path: &Path) -> io::Result<()> {
    let files: Vec<(String, Vec<u8>)> = (0..1_000)
        .map(|index| {
            let name = format!("programs/job-{index:04}.nc");
            let data = format!("%\n(job {index:04})\nM30\n%\n").into_bytes();
            (name, data)
        })
        .collect();
    let manifest = Manifest {
        version: 1,
        files: files
            .iter()
            .map(|(name, data)| declaration(name, data))
            .collect(),
        entry_program: "programs/job-0000.nc".to_owned(),
        required_tool_ids: vec!["T01".to_owned()],
    };
    let manifest = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
    let mut writer = Writer::create(path)?;
    writer.member("manifest.json", &manifest)?;
    for (name, data) in &files {
        writer.member(name, data)?;
    }
    writer.finish()
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    fs::write(path, bytes)
}
