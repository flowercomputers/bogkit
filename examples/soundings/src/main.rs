// G4-lite: the ESE quality probe — the single go/no-go for Soundings.
// Every proven Loupe number (z −2.0 separation, legible anchor axes) was earned on
// Voyage embeddings; this probe asks whether ese's static wordpiece geometry can
// reproduce them. Run: cargo run -p soundings
//
// PASS bar: (1) axis t-values order the concrete→abstract ladder mostly monotonically,
// (2) the planted off-voice sentence is the drift minimum with z < −1.25.
//
// `cargo run -p soundings -- serve` starts the v0 instrument (see serve.rs);
// `-- bench` runs the G2 corpus measurements.

mod gale;
mod lens;
mod pct;
mod anchors;
mod restyle;
mod serve;

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
fn cos(a: &[f32], b: &[f32]) -> f32 {
    dot(a, b) / (norm(a) * norm(b)).max(1e-9)
}
fn mean(vs: &[Vec<f32>]) -> Vec<f32> {
    let d = vs[0].len();
    let mut m = vec![0f32; d];
    for v in vs {
        for i in 0..d {
            m[i] += v[i];
        }
    }
    for x in m.iter_mut() {
        *x /= vs.len() as f32;
    }
    m
}
fn embed(s: &str) -> Vec<f32> {
    ese::encode_single(s).to_vec()
}
fn embed_all(xs: &[&str]) -> Vec<Vec<f32>> {
    xs.iter().map(|s| embed(s)).collect()
}

fn axis_probe(name: &str, neg: &[&str], pos: &[&str], ladder: &[(&str, &str)]) {
    let a = mean(&embed_all(neg));
    let b = mean(&embed_all(pos));
    let ab: Vec<f32> = b.iter().zip(&a).map(|(x, y)| x - y).collect();
    let ab2 = dot(&ab, &ab);
    println!("\n== axis: {name} ==");
    let mut scored: Vec<(f32, &str, &str)> = ladder
        .iter()
        .map(|(label, s)| {
            let x = embed(s);
            let xa: Vec<f32> = x.iter().zip(&a).map(|(v, w)| v - w).collect();
            let t = (dot(&xa, &ab) / ab2).clamp(0.0, 1.0);
            (t, *label, *s)
        })
        .collect();
    scored.sort_by(|p, q| p.0.partial_cmp(&q.0).unwrap());
    for (t, label, s) in &scored {
        println!("  t={t:.3}  [{label}]  {}", &s[..s.len().min(58)]);
    }
}

fn drift_probe(doc: &[&str], plant_idx: usize) {
    let vecs = embed_all(doc);
    let sims: Vec<f32> = (0..vecs.len())
        .map(|i| {
            let mut c = vec![0f32; vecs[0].len()];
            for (j, v) in vecs.iter().enumerate() {
                if j != i {
                    for k in 0..c.len() {
                        c[k] += v[k];
                    }
                }
            }
            cos(&vecs[i], &c)
        })
        .collect();
    let m = sims.iter().sum::<f32>() / sims.len() as f32;
    let sd = (sims.iter().map(|s| (s - m).powi(2)).sum::<f32>() / sims.len() as f32).sqrt();
    println!("\n== drift probe (planted off-voice at index {plant_idx}) ==");
    let mut min_i = 0;
    for (i, s) in sims.iter().enumerate() {
        let z = (s - m) / sd.max(1e-9);
        if *s < sims[min_i] {
            min_i = i;
        }
        println!("  z={z:+.2}  cos={s:.3}  {}", &doc[i][..doc[i].len().min(58)]);
    }
    let z_min = (sims[min_i] - m) / sd.max(1e-9);
    let caught = min_i == plant_idx && z_min < -1.25;
    println!(
        "  → min at index {min_i} (z={z_min:.2}) — plant {} {}",
        if min_i == plant_idx { "IS the min" } else { "is NOT the min" },
        if caught { "· CAUGHT ✓" } else { "· NOT CAUGHT ✗" }
    );
}

// ---------- G2 bench: corpus encode / index / REOPEN / kNN quality ----------
#[derive(serde::Deserialize)]
struct CanonRow {
    #[allow(dead_code)]
    book: u32,
    title: String,
    text: String,
}

fn bench() {
    use anny::metric::Cosine;
    use fold::pipeline::{Keyed, Map, terminal};
    use fold::stream::KeyedStream;
    const DIM: usize = ese::DIMENSIONS;

    let raw = std::fs::read_to_string("data/canon.jsonl").expect("run scripts/prep_corpus.py first");
    let rows: Vec<CanonRow> = raw.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    println!("canon: {} sentences · dim {}", rows.len(), DIM);

    // 1. raw encode throughput over the real corpus
    let t = std::time::Instant::now();
    let mut checksum = 0f32;
    for r in rows.iter() {
        checksum += ese::encode_single(&r.text)[0];
    }
    let enc_s = t.elapsed().as_secs_f64();
    println!("encode all: {:.1}s ({:.0}/s) [checksum {checksum:.3}]", enc_s, rows.len() as f64 / enc_s);

    // 2. build the canon stream: Keyed<u32,String> -> (Hnsw over ese, Table)
    let db = std::path::Path::new("data/canon.db");
    let snap = std::path::Path::new("data/canon.hnswsnap");
    let _ = std::fs::remove_dir_all(db);
    let _ = std::fs::remove_file(snap);
    let pipeline = || {
        (
            Map::new(
                |d: &Keyed<u32, String>| Keyed::new(d.key, ese::encode_single(&d.val)),
                terminal::search::Hnsw::<u32, f32, Cosine, DIM>::new("vecs", Cosine, 42)
                    .with_graph_snapshot(snap),
            ),
            terminal::Table::<u32, String>::new("docs"),
        )
    };
    let t = std::time::Instant::now();
    {
        let mut st = KeyedStream::new(db, pipeline());
        for (i, chunk) in rows.chunks(8192).enumerate() {
            let base = i * 8192;
            st.wtx(|tx| {
                for (j, r) in chunk.iter().enumerate() {
                    tx.upsert(&((base + j) as u32), &r.text);
                }
            });
            if (i + 1) % 4 == 0 {
                println!("  indexed {} ({:.0}/s)", base + chunk.len(), (base + chunk.len()) as f64 / t.elapsed().as_secs_f64());
            }
        }
        let ins_s = t.elapsed().as_secs_f64();
        println!("index build: {:.1}s ({:.0}/s)", ins_s, rows.len() as f64 / ins_s);
        let t = std::time::Instant::now();
        st.rtx(|(vecs, _)| vecs.save_graph()).expect("snapshot save");
        println!(
            "graph snapshot save: {:.2}s ({} bytes)",
            t.elapsed().as_secs_f64(),
            std::fs::metadata(snap).map(|m| m.len()).unwrap_or(0)
        );
    } // stream drops here

    // 3. REOPEN — with the graph snapshot (Séance, bogkit PR #6) this
    // fast-loads instead of rebuilding the HNSW row-by-row
    let t = std::time::Instant::now();
    let st = KeyedStream::new(db, pipeline());
    println!("REOPEN: {:.2}s  ← the cold-start number (snapshot fast-load)", t.elapsed().as_secs_f64());

    // 4. nearest-voice quality: eyeball top-3 canon hits for probe sentences
    let probes = [
        "The whale surfaced beside the boat, vast and indifferent as weather.",
        "I went to the woods because I wished to live deliberately.",
        "It is a truth universally acknowledged that a rich man wants a wife.",
        "The fog rolled in over Twin Peaks just after dark.",
    ];
    let title_of = |id: u32| rows.get(id as usize).map(|r| r.title.as_str()).unwrap_or("?");
    st.rtx(|(vecs, docs)| {
        for p in probes {
            println!("\nnearest voices for: {p:?}");
            let t = std::time::Instant::now();
            let hits = vecs.search(&ese::encode_single(p));
            let dt = t.elapsed();
            for h in hits.into_iter().take(3) {
                let text: String = docs.get(&h.val).unwrap_or_default();
                println!("  {:.3} [{}] {}", h.score, title_of(h.val), &text[..text.len().min(66)]);
            }
            println!("  (kNN in {dt:?})");
        }
    });
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("bench") => return bench(),
        Some("serve") => return serve::run(),
        Some("anchors") => return anchors::print_bench(),
        _ => {}
    }
    let t0 = std::time::Instant::now();
    let _warm = embed("warmup");
    println!("ese dim = {} · first encode {:?}", ese::DIMENSIONS, t0.elapsed());
    let t1 = std::time::Instant::now();
    let n = 1000;
    for i in 0..n {
        let _ = embed(if i % 2 == 0 { "the fog rolled in over the city" } else { "resilience is a rent we pay" });
    }
    println!("encode throughput: {:.0}/s", n as f64 / t1.elapsed().as_secs_f64());

    axis_probe(
        "concrete —— abstract (SENTENCE anchors)",
        &[
            "She wiped the counter and stacked the blue ceramic bowls.",
            "The bus driver counted quarters into a paper cup.",
            "Rain dripped from the fire escape onto the trash bags below.",
            "He tied his boots and zipped the canvas jacket to his chin.",
        ],
        &[
            "Freedom is the capacity to author one's own life.",
            "Progress depends on institutions that outlive their founders.",
            "Meaning arises when suffering is given a purpose.",
            "Truth survives only where inquiry is unafraid.",
        ],
        &[
            ("very concrete", "The knife lay on the wooden cutting board next to two onions."),
            ("concrete", "She parked the truck outside the taqueria and counted her change."),
            ("mid", "He worked long hours because the family needed the money."),
            ("mid-abstract", "Their sacrifice meant something larger than the daily grind."),
            ("abstract", "Dignity is the quiet premise beneath every fair wage."),
            ("very abstract", "Justice, at last, is a promise a society makes to itself."),
        ],
    );

    axis_probe(
        "sad —— happy (SENTENCE anchors)",
        &[
            "He sat by the phone that never rang and felt the year drain out of him.",
            "The house was quiet in the way only a missing person can make it.",
            "She folded the small shirts one last time and closed the drawer.",
        ],
        &[
            "They cheered until their voices cracked and hugged strangers like family.",
            "The whole table burst out laughing before the toast could finish.",
            "She spun her daughter in the sunlight and the day felt endless.",
        ],
        &[
            ("very sad", "She wept alone in the empty apartment after the funeral."),
            ("sad", "The letter never came, and eventually he stopped checking."),
            ("neutral", "The train arrives at noon on weekdays and one on Sundays."),
            ("happy", "The kids ran laughing through the sprinkler all afternoon."),
            ("very happy", "We danced in the kitchen until midnight, giddy with the news."),
        ],
    );

    // the SF demo doc + the planted ChatGPT sentence (index 9)
    drift_probe(
        &[
            "The fog rolled in over Twin Peaks just after dark, and the city went quiet the way it does before bad news.",
            "Outside the taqueria on Mission, a driver sat with his hazards on, doing the arithmetic of a night that owed him money.",
            "Four dollars, fifteen minutes.",
            "He took the ride anyway.",
            "He was standing in the poetry aisle of the bookstore on Valencia, holding her umbrella like an apology.",
            "She had rehearsed this conversation for three weeks, in showers and stairwells and the long fluorescent silence of the night bus, and now every word of it was gone.",
            "I have watched this city fall down and pick itself up four times in ten years.",
            "Resilience is not a virtue here; it is a rent we pay.",
            "What worries me is not the falling — it is who we ask to do the catching, and what we pay them for it.",
            "The optimal strategy was to leverage the moment efficiently.",
        ],
        9,
    );
}
