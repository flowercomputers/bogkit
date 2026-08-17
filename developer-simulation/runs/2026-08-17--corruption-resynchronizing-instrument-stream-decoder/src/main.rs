use std::hint::black_box;
use std::time::Instant;

use instrument_decoder_trial::{
    AppendSearchBaseline, BenchmarkCorpus, ChunkSchedule, Decoder, encode_frame,
    run_clean_workload, run_damaged_workload, run_property_cases,
};

fn main() {
    let command = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "demo".to_string());
    match command.as_str() {
        "demo" => demo(),
        "representative" => representative(),
        "full" => full(),
        "property" => property(),
        "bench" => bench(),
        _ => {
            eprintln!("usage: instrument-decoder-trial [demo|representative|full|property|bench]");
            std::process::exit(2);
        }
    }
}

fn demo() {
    let mut bytes = encode_frame(31, b"damaged");
    if let Some(marker) = bytes.last_mut() {
        *marker = 0x7E;
    }
    bytes.extend_from_slice(&encode_frame(32, b"follower"));

    let mut baseline = AppendSearchBaseline::new();
    let baseline_result = baseline.feed(1, &bytes);
    let mut candidate = Decoder::new();
    let candidate_result = candidate.feed(1, &bytes);
    println!(
        "demo baseline_frames={} baseline_resets={} baseline_peak={} candidate_frames={} candidate_diagnostics={} candidate_retained={}",
        baseline_result.frames.len(),
        baseline.reset_count(),
        baseline.max_retained_bytes(),
        candidate_result.frames.len(),
        candidate_result.diagnostics.len(),
        candidate.retained_for(1),
    );
    println!("demo candidate={candidate_result:?}");
}

fn representative() {
    run_workload("representative", 100_000, 2_500);
}

fn full() {
    run_workload("full", 1_000_000, 25_000);
}

fn run_workload(label: &str, clean_frames: usize, damaged_events: usize) {
    const SEED: u64 = 0xA17E_2026;
    println!(
        "{label} config connections=64 clean_frames={clean_frames} damaged_events={damaged_events} seed=0x{SEED:08X} short_wire=192 max_wire=4096"
    );
    for (name, schedule) in [
        ("whole-frame", ChunkSchedule::WholeFrame),
        ("one-byte", ChunkSchedule::OneByte),
        ("irregular", ChunkSchedule::Irregular(SEED)),
    ] {
        let started = Instant::now();
        let summary = run_clean_workload(clean_frames, schedule);
        println!(
            "clean schedule={name} elapsed_s={:.6} summary={summary:?}",
            started.elapsed().as_secs_f64()
        );
        black_box(summary);
    }
    for (name, schedule) in [
        ("whole-frame", ChunkSchedule::WholeFrame),
        ("one-byte", ChunkSchedule::OneByte),
        ("irregular", ChunkSchedule::Irregular(SEED)),
    ] {
        let started = Instant::now();
        let damage = run_damaged_workload(damaged_events, schedule);
        println!(
            "damaged schedule={name} elapsed_s={:.6} summary={damage:?}",
            started.elapsed().as_secs_f64()
        );
        black_box(damage);
    }
}

fn property() {
    let cases = std::env::args()
        .nth(2)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100_000);
    let started = Instant::now();
    let summary = run_property_cases(cases, 32 * 1_024, 0xA17E_2026);
    println!(
        "property elapsed_s={:.6} cap_bytes=32768 seed=0xA17E2026 summary={summary:?}",
        started.elapsed().as_secs_f64()
    );
    black_box(summary);
}

fn bench() {
    let frames = std::env::args()
        .nth(2)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100_000);
    let frames_per_pass = frames.clamp(1, 10_000);
    let passes = frames.div_ceil(frames_per_pass);
    let corpus = BenchmarkCorpus::build(frames_per_pass);
    println!(
        "bench config requested_frames={frames} prebuilt_frames={frames_per_pass} passes={passes} chunk_bytes=4096 warmups=1 measured_runs=3"
    );
    black_box(corpus.run_baseline(1));
    black_box(corpus.run_candidate(1));

    let baseline_rates = timed_runs("baseline", || corpus.run_baseline(passes));
    let candidate_rates = timed_runs("candidate", || corpus.run_candidate(passes));
    let baseline_slowest = baseline_rates.into_iter().fold(f64::INFINITY, f64::min);
    let candidate_slowest = candidate_rates.into_iter().fold(f64::INFINITY, f64::min);
    println!(
        "bench slower_of_three baseline_mib_s={baseline_slowest:.3} candidate_mib_s={candidate_slowest:.3} ratio={:.6}",
        candidate_slowest / baseline_slowest
    );
}

fn timed_runs(
    name: &str,
    mut run: impl FnMut() -> instrument_decoder_trial::BaselineSummary,
) -> [f64; 3] {
    let mut rates = [0.0; 3];
    for (index, rate) in rates.iter_mut().enumerate() {
        let started = Instant::now();
        let summary = run();
        let seconds = started.elapsed().as_secs_f64();
        let measured_bytes = u32::try_from(summary.wire_bytes).unwrap_or(u32::MAX);
        *rate = f64::from(measured_bytes) / (1024.0 * 1024.0) / seconds;
        println!(
            "bench parser={name} run={} elapsed_s={seconds:.6} mib_s={:.3} frames={} bytes={} digest=0x{:016X} diagnostics={} resets={} max_retained={}",
            index + 1,
            *rate,
            summary.frames_emitted,
            summary.wire_bytes,
            summary.candidate_digest,
            summary.diagnostics,
            summary.resets,
            summary.max_retained,
        );
        black_box(summary);
    }
    rates
}
