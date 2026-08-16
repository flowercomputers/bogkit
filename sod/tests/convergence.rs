//! Convergence property tests: SOD-4 / SOD-6 / SOD-8.
//!
//! N replicas, random interleaved local commits, random pairwise syncs, and
//! randomly interrupted syncs. The invariant checked after every step: any
//! two replicas with equal version vectors have byte-identical views and
//! equal watermarks. Every case ends with full anti-entropy rounds and
//! asserts global convergence.

use sod::engine::MemEngine;
use sod::store::MemLog;
use sod::{Replica, ReplicaId, Session, sync_pair};

/// Deterministic xorshift64* — no external RNG dep, reproducible per seed.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E3779B97F4A7C15).max(1))
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const SCHEMA: u32 = 1;

fn check_pairwise(replicas: &[Replica<MemEngine, MemLog>], step: usize, seed: u64) {
    for i in 0..replicas.len() {
        for j in i + 1..replicas.len() {
            if replicas[i].vector() == replicas[j].vector() {
                assert_eq!(
                    replicas[i].engine().view_bytes(),
                    replicas[j].engine().view_bytes(),
                    "seed {seed} step {step}: equal vectors, different views ({i} vs {j})"
                );
                assert_eq!(
                    replicas[i].watermark(),
                    replicas[j].watermark(),
                    "seed {seed} step {step}: equal vectors, different watermarks ({i} vs {j})"
                );
            }
        }
    }
}

/// A sync session between `a` and `b` that dies after delivering only some
/// of each side's outbound messages (SOD-6: interruption is always safe).
fn interrupted_sync(
    a: &mut Replica<MemEngine, MemLog>,
    b: &mut Replica<MemEngine, MemLog>,
    rng: &mut Rng,
) {
    let mut sa = Session::new(SCHEMA);
    let mut sb = Session::new(SCHEMA);
    let hello_a = sa.hello(a);
    let hello_b = sb.hello(b);
    let out_b = sb.on_msg(b, hello_a).unwrap(); // b's frames for a
    let out_a = sa.on_msg(a, hello_b).unwrap(); // a's frames for b
    let deliver_to_a = rng.below(out_b.len() + 1);
    let deliver_to_b = rng.below(out_a.len() + 1);
    for m in out_b.into_iter().take(deliver_to_a) {
        sa.on_msg(a, m).unwrap();
    }
    for m in out_a.into_iter().take(deliver_to_b) {
        sb.on_msg(b, m).unwrap();
    }
    // session abandoned here — no completion handshake ever happens
}

fn run_case(seed: u64) {
    let mut rng = Rng::new(seed);
    let n = 2 + rng.below(4); // 2..=5 replicas
    let mut replicas: Vec<Replica<MemEngine, MemLog>> = (0..n)
        .map(|i| {
            Replica::open(ReplicaId([i as u8 + 1; 16]), MemLog::new(), MemEngine::new()).unwrap()
        })
        .collect();

    for step in 0..200 {
        match rng.below(10) {
            0..=5 => {
                // local commit: random datum from a small alphabet, nonzero mult
                let who = rng.below(n);
                let datum = vec![b'k', rng.below(8) as u8];
                let mult = [1i64, 2, -1, -2][rng.below(4)];
                let event_time = rng.below(10_000) as u64;
                replicas[who]
                    .commit(vec![(datum, mult)], event_time)
                    .unwrap();
            }
            6..=8 => {
                let i = rng.below(n);
                let j = rng.below(n);
                if i != j {
                    let (a, b) = borrow_two(&mut replicas, i, j);
                    sync_pair(a, b, SCHEMA).unwrap();
                }
            }
            _ => {
                let i = rng.below(n);
                let j = rng.below(n);
                if i != j {
                    let (a, b) = borrow_two(&mut replicas, i, j);
                    interrupted_sync(a, b, &mut rng);
                }
            }
        }
        check_pairwise(&replicas, step, seed);
    }

    // Full anti-entropy: everyone syncs with replica 0, twice — the second
    // round relays what the first round taught replica 0.
    for _ in 0..2 {
        for j in 1..n {
            let (a, b) = borrow_two(&mut replicas, 0, j);
            sync_pair(a, b, SCHEMA).unwrap();
        }
    }
    for j in 1..n {
        assert_eq!(
            replicas[0].vector(),
            replicas[j].vector(),
            "seed {seed}: vectors did not converge"
        );
    }
    check_pairwise(&replicas, usize::MAX, seed);
    for j in 1..n {
        assert_eq!(
            replicas[0].engine().view_bytes(),
            replicas[j].engine().view_bytes(),
            "seed {seed}: views did not converge"
        );
    }
}

fn borrow_two<T>(v: &mut [T], i: usize, j: usize) -> (&mut T, &mut T) {
    assert_ne!(i, j);
    if i < j {
        let (lo, hi) = v.split_at_mut(j);
        (&mut lo[i], &mut hi[0])
    } else {
        let (lo, hi) = v.split_at_mut(i);
        (&mut hi[0], &mut lo[j])
    }
}

#[test]
fn convergence_100_random_cases() {
    for seed in 0..100 {
        run_case(seed);
    }
}
