use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

use reach_rollup_lab::{
    AccuracyRun, AdversarialVerification, BenchmarkResult, LoadErrorSummary, LoadVerification,
    run_accuracy_matrix, run_adversarial_verification, run_candidate_benchmark,
    run_exact_benchmark, run_load_verification,
};

const HASH_SEED: u64 = 0xbb67_ae85_84ca_a73b;
const TUNING_GENERATOR_SEED: u64 = 0x1319_8a2e_0370_7344;
const HELD_OUT_GENERATOR_SEED: u64 = 0x082e_fa98_ec4e_6c89;

fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: &[String]) -> Result<(), String> {
    match arguments {
        [flag] if flag == "--help" || flag == "-h" => {
            print_help();
            Ok(())
        }
        [command] if command == "bench-exact" => {
            print_benchmark(&run_exact_benchmark().map_err(|error| error.to_string())?);
            Ok(())
        }
        [command] if command == "bench-candidate" => {
            print_benchmark(&run_candidate_benchmark().map_err(|error| error.to_string())?);
            Ok(())
        }
        [command, evidence, temporary] if command == "demo" => {
            run_demo(Path::new(evidence), Path::new(temporary))
        }
        [] => {
            print_help();
            Ok(())
        }
        [command, ..] => Err(format!("unknown command or wrong arguments: {command}")),
    }
}

fn print_help() {
    println!(
        "reach-rollup-lab\n\n  demo <evidence-dir> <temporary-dir>\n  bench-exact\n  bench-candidate"
    );
}

fn print_benchmark(result: &BenchmarkResult) {
    println!("mode={}", result.mode);
    println!("records={}", result.record_count);
    println!("buckets={}", result.bucket_count);
    if let Some(unique) = result.unique_bucket_occurrences {
        println!("unique_bucket_occurrences={unique}");
    }
    if let Some(size) = result.max_serialized_bucket_size {
        println!("max_serialized_bucket_size={size}");
    }
    println!("digest={:016x}", result.digest);
}

fn run_demo(evidence: &Path, temporary: &Path) -> Result<(), String> {
    std::fs::create_dir_all(evidence).map_err(|error| error.to_string())?;
    let tuning = run_accuracy_matrix(HASH_SEED, TUNING_GENERATOR_SEED);
    let held_out = run_accuracy_matrix(HASH_SEED, HELD_OUT_GENERATOR_SEED);
    require_accuracy_gate("tuning", &tuning)?;
    require_accuracy_gate("held-out", &held_out)?;
    write_atomic(
        &evidence.join("accuracy-tuning.tsv"),
        tuning.to_tsv().as_bytes(),
    )?;
    write_atomic(
        &evidence.join("accuracy-heldout.tsv"),
        held_out.to_tsv().as_bytes(),
    )?;

    let load = run_load_verification(temporary).map_err(|error| error.to_string())?;
    let adversarial = run_adversarial_verification().map_err(|error| error.to_string())?;
    write_atomic(&evidence.join("load-report.tsv"), &load.report_bytes)?;
    let mut summary = String::new();
    writeln!(summary, "format_version=1").unwrap();
    writeln!(summary, "state_file_version=2").unwrap();
    writeln!(summary, "hash_algorithm=splitmix64-avalanche-v1").unwrap();
    writeln!(summary, "hash_seed={HASH_SEED:#018x}").unwrap();
    writeln!(
        summary,
        "load_generator_seed={:#018x}",
        0x243f_6a88_85a3_08d3_u64
    )
    .unwrap();
    writeln!(
        summary,
        "tuning_generator_seed={TUNING_GENERATOR_SEED:#018x}"
    )
    .unwrap();
    writeln!(
        summary,
        "held_out_generator_seed={HELD_OUT_GENERATOR_SEED:#018x}"
    )
    .unwrap();
    append_accuracy_summary(&mut summary, "tuning", &tuning);
    append_accuracy_summary(&mut summary, "heldout", &held_out);
    append_load_summary(&mut summary, &load);
    append_adversarial_summary(&mut summary, &adversarial);
    write_atomic(&evidence.join("summary.txt"), summary.as_bytes())?;
    print!("{summary}");
    Ok(())
}

fn append_load_summary(summary: &mut String, load: &LoadVerification) {
    writeln!(summary, "load_records={}", load.record_count).unwrap();
    writeln!(
        summary,
        "load_unique_bucket_occurrences={}",
        load.unique_bucket_occurrences
    )
    .unwrap();
    writeln!(summary, "load_buckets={}", load.bucket_count).unwrap();
    writeln!(summary, "load_1k_buckets={}", load.cardinality_1k_buckets).unwrap();
    writeln!(summary, "load_5k_buckets={}", load.cardinality_5k_buckets).unwrap();
    writeln!(summary, "load_10k_buckets={}", load.cardinality_10k_buckets).unwrap();
    writeln!(
        summary,
        "state_size_max={}",
        load.max_serialized_bucket_size
    )
    .unwrap();
    writeln!(
        summary,
        "total_serialized_bucket_state={}",
        load.total_serialized_bucket_state
    )
    .unwrap();
    writeln!(
        summary,
        "duplicates_byte_identical={}",
        load.duplicates_byte_identical
    )
    .unwrap();
    writeln!(
        summary,
        "direct_sharded_byte_identical={}",
        load.direct_and_sharded_byte_identical
    )
    .unwrap();
    writeln!(
        summary,
        "merge_permutations={}",
        load.merge_permutation_digests.len()
    )
    .unwrap();
    writeln!(
        summary,
        "merge_permutations_all_equal={}",
        load.merge_permutation_digests
            .iter()
            .all(|digest| *digest == load.state_digest)
    )
    .unwrap();
    writeln!(
        summary,
        "repeated_state_byte_identical={}",
        load.repeated_state_byte_identical
    )
    .unwrap();
    writeln!(
        summary,
        "repeated_report_byte_identical={}",
        load.repeated_report_byte_identical
    )
    .unwrap();
    writeln!(summary, "state_digest={:016x}", load.state_digest).unwrap();
    writeln!(summary, "report_digest={:016x}", load.report_digest).unwrap();
    writeln!(summary, "summary_digest={:016x}", load.summary_digest).unwrap();
    append_load_error_summary(summary, &load.errors);
}

fn append_load_error_summary(summary: &mut String, errors: &LoadErrorSummary) {
    writeln!(
        summary,
        "load_error_median={:.9}",
        errors.median_absolute_error
    )
    .unwrap();
    writeln!(summary, "load_error_p95={:.9}", errors.p95_absolute_error).unwrap();
    writeln!(
        summary,
        "load_error_worst={:.9}",
        errors.worst_absolute_error
    )
    .unwrap();
    writeln!(summary, "load_positive_count={}", errors.positive_count).unwrap();
    writeln!(summary, "load_negative_count={}", errors.negative_count).unwrap();
    writeln!(
        summary,
        "load_positive_mean={:.9}",
        errors.positive_mean_error
    )
    .unwrap();
    writeln!(
        summary,
        "load_negative_mean={:.9}",
        errors.negative_mean_error
    )
    .unwrap();
}

fn append_adversarial_summary(summary: &mut String, adversarial: &AdversarialVerification) {
    writeln!(
        summary,
        "adversarial_id_patterns={}",
        adversarial.id_pattern_cases
    )
    .unwrap();
    writeln!(
        summary,
        "adversarial_worst_absolute_error={:.9}",
        adversarial.worst_absolute_error
    )
    .unwrap();
    writeln!(
        summary,
        "reverse_order_byte_identical={}",
        adversarial.reverse_order_byte_identical
    )
    .unwrap();
    writeln!(
        summary,
        "thousand_duplicates_byte_identical={}",
        adversarial.thousand_duplicates_byte_identical
    )
    .unwrap();
    writeln!(
        summary,
        "hot_shard_merge_byte_identical={}",
        adversarial.hot_shard_merge_byte_identical
    )
    .unwrap();
    writeln!(
        summary,
        "empty_input_is_empty={}",
        adversarial.empty_input_is_empty
    )
    .unwrap();
    writeln!(
        summary,
        "one_bucket_is_one_bucket={}",
        adversarial.one_bucket_is_one_bucket
    )
    .unwrap();
}

fn require_accuracy_gate(label: &str, run: &AccuracyRun) -> Result<(), String> {
    if run.summary.median_absolute_error > 0.015
        || run.summary.p95_absolute_error > 0.035
        || run.summary.worst_absolute_error > 0.08
    {
        return Err(format!("{label} accuracy gate failed: {:?}", run.summary));
    }
    Ok(())
}

fn append_accuracy_summary(output: &mut String, label: &str, run: &AccuracyRun) {
    writeln!(
        output,
        "{label}_median_absolute_error={:.9}",
        run.summary.median_absolute_error
    )
    .unwrap();
    writeln!(
        output,
        "{label}_p95_absolute_error={:.9}",
        run.summary.p95_absolute_error
    )
    .unwrap();
    writeln!(
        output,
        "{label}_worst_absolute_error={:.9}",
        run.summary.worst_absolute_error
    )
    .unwrap();
    writeln!(
        output,
        "{label}_positive_count={}",
        run.summary.positive_count
    )
    .unwrap();
    writeln!(
        output,
        "{label}_negative_count={}",
        run.summary.negative_count
    )
    .unwrap();
    writeln!(
        output,
        "{label}_positive_mean={:.9}",
        run.summary.positive_mean_error
    )
    .unwrap();
    writeln!(
        output,
        "{label}_negative_mean={:.9}",
        run.summary.negative_mean_error
    )
    .unwrap();
    writeln!(
        output,
        "{label}_worst_positive_case={:?}",
        run.summary.worst_positive_case
    )
    .unwrap();
    writeln!(
        output,
        "{label}_worst_negative_case={:?}",
        run.summary.worst_negative_case
    )
    .unwrap();
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())
}
