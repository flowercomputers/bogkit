//! Tompkins Square Audio Search — a searchable, multimodal memory of a park.
//!
//! Ask for words, weather, city sounds, or bird calls, then listen at the
//! moment the archive remembers. See `README.md` for the architecture and
//! `docs/adr/` for the decisions behind it.
//!
//! Subcommands:
//!
//! ```text
//! manifest [all|295]   freeze the canonical corpus manifest (milestone 1)
//! ```

// The data model is written out in full ahead of the pipeline stages that
// consume it, so parts of it are legitimately unused between milestones.
#![allow(dead_code)]

mod domain;
mod manifest;
mod master;
mod prepared;
mod rank;
mod search;
mod server;
mod store;
mod timeline;
mod timeutil;

use std::collections::BTreeMap;
use std::path::PathBuf;

use domain::StreamId;
use manifest::{ManifestProvenance, Selection};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let rest = if args.is_empty() { &args[..] } else { &args[1..] };

    let result = match cmd {
        "manifest" => cmd_manifest(rest),
        "timeline" => cmd_timeline(rest),
        "commit" => cmd_commit(rest),
        "forget" => cmd_forget(rest),
        "serve" => cmd_serve(rest),
        "help" | "--help" | "-h" => {
            usage();
            Ok(())
        }
        other => Err(format!("unknown subcommand {other:?}; try `help`")),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn usage() {
    println!(
        "tompkins-audio-search\n\
         \n\
         usage: cargo run -p tompkins-audio-search -- <subcommand>\n\
         \n\
         manifest [all|295] [--out DIR] [--catalog DIR]\n\
         \x20   freeze the canonical corpus manifest from the local cross-reference\n\
         \x20   catalog. reads no media and touches no cloud resources.\n\
         \n\
         timeline <streamId> [--pts FILE] [--assets FILE] [--asset-minutes N]\n\
         \x20   build the source-to-playback map from the segment index, refine it\n\
         \x20   with decoded PTS, adopt the compacted asset table, and verify that\n\
         \x20   landmark seeks round-trip.\n\
         \n\
         commit <streamId> [--prepared DIR] [--db PATH]\n\
         \x20   commit prepared inference batches in short atomic transactions.\n\
         \n\
         serve [--port N] [--db PATH] [--clap ADDR | --no-clap]\n\
         \x20   serve the search and listening UI.\n\
         \n\
         forget <streamId> [--db PATH] [--purge]\n\
         \x20   retract every record for a stream from every index. --purge also\n\
         \x20   deletes its cached segments, assets, batches and timeline.\n"
    );
}

/// Milestone 2: build and verify the source-to-playback map.
fn cmd_timeline(args: &[String]) -> Result<(), String> {
    let stream_id: StreamId = args
        .first()
        .ok_or("timeline needs a stream id")?
        .parse()
        .map_err(|_| "stream id must be numeric")?;

    let mut pts_paths: Vec<PathBuf> = Vec::new();
    let mut asset_tables: Vec<PathBuf> = Vec::new();
    let mut asset_minutes: u64 = 30;
    let mut index_dir = PathBuf::from("data/segments");
    let mut out_dir = PathBuf::from("data/timeline");

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--pts" => {
                pts_paths.push(PathBuf::from(args.get(i + 1).ok_or("--pts needs a file")?));
                i += 2;
            }
            "--assets" => {
                asset_tables.push(PathBuf::from(args.get(i + 1).ok_or("--assets needs a file")?));
                i += 2;
            }
            "--asset-minutes" => {
                asset_minutes = args
                    .get(i + 1)
                    .ok_or("--asset-minutes needs a number")?
                    .parse()
                    .map_err(|_| "--asset-minutes must be numeric")?;
                i += 2;
            }
            "--index-dir" => {
                index_dir = PathBuf::from(args.get(i + 1).ok_or("--index-dir needs a directory")?);
                i += 2;
            }
            "--out" => {
                out_dir = PathBuf::from(args.get(i + 1).ok_or("--out needs a directory")?);
                i += 2;
            }
            other => return Err(format!("unknown flag {other:?}")),
        }
    }

    // With nothing named explicitly, take every window fetched so far. A
    // stream is compacted in pieces, and each piece writes its own pts-*.json
    // and assets-*.json; the timeline is the union of them.
    if pts_paths.is_empty() && asset_tables.is_empty() {
        let dir = PathBuf::from(format!("data/assets/{stream_id}"));
        if dir.exists() {
            let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
                .map_err(|e| format!("{}: {e}", dir.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .collect();
            found.sort();
            for p in found {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                if name.starts_with("pts-") && name.ends_with(".json") {
                    pts_paths.push(p);
                } else if name.starts_with("assets-") && name.ends_with(".json") {
                    asset_tables.push(p);
                }
            }
        }
    }

    let index_path = index_dir.join(format!("stream-{stream_id}-stream_4.json"));
    let index = timeline::SegmentIndex::read_json(&index_path)?;
    println!(
        "segment index: {} objects, numbering {:?}..{:?}",
        index.object_count, index.first_segment, index.last_segment
    );

    let nominal = timeline::Timeline::from_index(&index);
    println!(
        "\n=== nominal timeline (every segment assumed {} AAC frames) ===",
        timeline::NOMINAL_SEGMENT_TICKS / timeline::TICKS_PER_AAC_FRAME
    );
    println!("entries            {}", nominal.entries.len());
    println!(
        "gaps               {} ({} segments missing)",
        nominal.gap_count(),
        nominal.missing_segment_count()
    );
    println!(
        "extent             {:.3} h  (assumed, not measured)",
        nominal.total_ms() as f64 / 3_600_000.0
    );

    let mut actual = nominal.clone();
    if !pts_paths.is_empty() {
        let mut probes = std::collections::BTreeMap::new();
        for p in &pts_paths {
            let batch = timeline::read_pts_probes(p)?;
            println!("read {} PTS probes from {}", batch.len(), p.display());
            probes.extend(batch);
        }
        println!("{} decoded segments in total", probes.len());
        actual.apply_pts(&probes);

        let d = actual.drift_report(&nominal);
        println!("\n=== decoded timeline ===");
        println!("segments with PTS  {}", d.pts_segments);
        println!("still assumed      {}", d.assumed_segments);
        println!(
            "extent             {:.3} h  (measured over the probed range)",
            actual.total_ms() as f64 / 3_600_000.0
        );
        println!(
            "worst drift        {} ms at segment {}  <-- the seek error nominal durations would have caused",
            d.max_abs_ms, d.at_media_sequence
        );
        println!("discontinuities    {}", actual.discontinuity_count());
        if let Some(u) = actual.position_uncertainty() {
            println!(
                "\n!! {} segments before the decoded window were never fetched, so their\n\
                 \x20  durations are assumed. At the measured rate ({:+.1} ticks/segment) the\n\
                 \x20  window is displaced by about {:+.1} minutes: times *inside* it are exact,\n\
                 \x20  its offset from the start of the stream is not.",
                u.assumed_prefix_segments,
                u.mean_error_ticks_per_segment,
                u.estimated_offset_ms as f64 / 60_000.0,
            );
        }
    } else {
        println!("\n(no --pts given: the timeline stays assumed, and must not be trusted for seeking)");
    }

    // the files that exist win over any planned chunking
    match asset_tables.is_empty() {
        false => {
            let mut table = Vec::new();
            for p in &asset_tables {
                table.extend(timeline::read_asset_table(p)?);
            }
            actual.apply_asset_table(&table)?;
            println!(
                "\n=== assets (from {} table(s)) ===",
                asset_tables.len()
            );
        }
        true => {
            actual.assign_assets(asset_minutes * 60 * timeline::TICKS_PER_SECOND);
            println!("\n=== assets ({asset_minutes}-minute target, PLANNED not measured) ===");
        }
    }
    println!("count              {}", actual.assets.len());
    for a in actual.assets.iter().take(6) {
        println!(
            "  {}  seq {}..{}  {:.3}..{:.3} s",
            a.asset_id,
            a.first_media_sequence,
            a.last_media_sequence,
            a.stream_start_ticks as f64 / timeline::TICKS_PER_SECOND as f64,
            a.stream_end_ticks as f64 / timeline::TICKS_PER_SECOND as f64,
        );
    }
    if actual.assets.len() > 6 {
        println!("  ... and {} more", actual.assets.len() - 6);
    }

    // ---- landmark verification -------------------------------------------
    // ten positions spread across the stream, each resolved and then checked
    // against the entry that actually contains it
    println!("\n=== landmark seeks ===");
    let total = actual.total_ticks;
    let mut failures = 0;
    for i in 0..10u64 {
        let t = total * i / 10;
        match actual.resolve_ticks(t) {
            Some(r) => {
                let entry = actual
                    .entries
                    .iter()
                    .find(|e| e.media_sequence == r.media_sequence)
                    .expect("resolved to an entry that exists");
                let contains =
                    t >= entry.cumulative_start_ticks && t < entry.cumulative_end_ticks();
                if !contains {
                    failures += 1;
                }
                println!(
                    "  t={:>10.3}s -> seq {:>7}  asset {:<14} offset {:>9.3}s  {}{}",
                    t as f64 / timeline::TICKS_PER_SECOND as f64,
                    r.media_sequence,
                    r.asset_id.clone().unwrap_or_else(|| "-".into()),
                    r.asset_offset_ticks.unwrap_or(0) as f64
                        / timeline::TICKS_PER_SECOND as f64,
                    if contains { "ok" } else { "MISMATCH" },
                    if r.in_gap { "  (in gap)" } else { "" },
                );
            }
            None => {
                println!("  t={:>10.3}s -> unresolvable", t as f64 / 90_000.0);
                failures += 1;
            }
        }
    }

    let out_path = out_dir.join(format!("timeline-{stream_id}.json"));
    actual.write_json(&out_path)?;
    println!("\nwrote {}", out_path.display());

    // A small, authoritative asset table for the Python workers.
    //
    // compact.mjs records each asset's position relative to *the fetch*, which
    // is only the same thing as its position in the stream when the fetch
    // started at segment 0. Fetching a window out of the middle of a stream
    // made every downstream timestamp 167.6 h too early. The timeline knows the
    // real offsets, but at 234 MB it is far too heavy for a worker to parse, so
    // the resolved spans are written out separately.
    let resolved = out_dir.join(format!("assets-{stream_id}.json"));
    let payload = serde_json::json!({
        "streamId": stream_id,
        "note": "stream-relative asset positions resolved from the decoded timeline; \
                 authoritative over the fetch-relative values in data/assets/*/assets-*.json",
        "assets": actual.assets.iter().map(|a| serde_json::json!({
            "assetId": a.asset_id,
            "firstMediaSequence": a.first_media_sequence,
            "lastMediaSequence": a.last_media_sequence,
            // round the start up and the end down, so the advertised
            // millisecond range sits strictly inside the real tick range.
            // Truncating both would place the first window up to 89 ticks
            // *before* the asset begins, where the preceding segment has no
            // compacted audio and the record is refused.
            "streamStartMs": a.stream_start_ticks.div_ceil(timeline::TICKS_PER_MS),
            "streamEndMs": a.stream_end_ticks / timeline::TICKS_PER_MS,
        })).collect::<Vec<_>>(),
    });
    std::fs::write(&resolved, serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())? + "\n")
        .map_err(|e| format!("{}: {e}", resolved.display()))?;
    println!("wrote {}", resolved.display());

    let reread = timeline::Timeline::read_json(&out_path)?;
    if reread.total_ticks != actual.total_ticks || reread.entries.len() != actual.entries.len() {
        return Err("timeline did not survive a write/read round trip".into());
    }
    println!("verified: timeline round-trips");

    if failures > 0 {
        return Err(format!("{failures} landmark seek(s) failed"));
    }
    println!("verified: all 10 landmark seeks resolve to the containing segment");
    Ok(())
}

/// Milestone 1: freeze the canonical corpus manifest.
fn cmd_manifest(args: &[String]) -> Result<(), String> {
    let mut selection = Selection::AllTompkinsLinked;
    let mut out_dir = PathBuf::from("data/manifest");
    let mut catalog_dir = manifest::default_catalog_dir();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                out_dir = PathBuf::from(args.get(i + 1).ok_or("--out needs a directory")?);
                i += 2;
            }
            "--catalog" => {
                catalog_dir = PathBuf::from(args.get(i + 1).ok_or("--catalog needs a directory")?);
                i += 2;
            }
            other => {
                selection = Selection::parse(other)
                    .ok_or_else(|| format!("unknown selection {other:?}; try `all` or `295`"))?;
                i += 1;
            }
        }
    }

    let catalog_path = catalog_dir.join("oda_stream_catalog.csv");
    let links_path = catalog_dir.join("oda_stream_performance_links.csv");

    let catalog = manifest::load_catalog(&catalog_path)?;
    let links = manifest::load_links(&links_path)?;
    println!(
        "read {} catalog rows and {} performance links",
        catalog.len(),
        links.len()
    );

    let provenance = ManifestProvenance {
        catalog_path: catalog_path.display().to_string(),
        catalog_blake3: manifest::file_hash(&catalog_path)?,
        links_path: links_path.display().to_string(),
        links_blake3: manifest::file_hash(&links_path)?,
        performance_filter: manifest::TOMPKINS_FILTER.to_string(),
        selection,
    };

    let m = manifest::build(&catalog, &links, selection, provenance);
    let report = manifest::drift(&catalog, &links, &m);

    println!("\n=== corpus: {} ===", selection.as_str());
    println!("streams            {}", m.totals.stream_count);
    println!(
        "duration           {}  ({:.2} h)",
        m.totals.duration_hms(),
        m.totals.duration_hours()
    );
    println!(
        "ambiguous          {}  (also served a non-Tompkins performance)",
        m.totals.ambiguous_stream_count
    );
    println!("manifest hash      {}", m.manifest_hash);

    println!("\n{:<38} {:>5} {:>12}", "source name", "n", "hours");
    for (name, count, hours) in m.by_source_name() {
        println!("{name:<38} {count:>5} {hours:>12.2}");
    }

    println!("\n=== drift ===");
    println!(
        "linked without catalog row   {:?}",
        report.linked_without_catalog_row
    );
    println!(
        "duplicate catalog rows       {:?}",
        report.duplicate_catalog_rows
    );
    println!(
        "missing playlist url         {:?}",
        report.missing_playlist_url
    );
    println!("zero duration                {:?}", report.zero_duration);
    println!(
        "unprojectable assignments    {} streams",
        report.unprojectable_assignments.len()
    );
    for (id, others) in &report.ambiguous_membership {
        println!("ambiguous  {id}  also served {}", others.join(", "));
    }

    let manifest_path = out_dir.join(format!("corpus-{}.json", selection.as_str()));
    let drift_path = out_dir.join(format!("drift-{}.json", selection.as_str()));
    m.write_json(&manifest_path)?;
    std::fs::write(
        &drift_path,
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())? + "\n",
    )
    .map_err(|e| e.to_string())?;

    println!("\nwrote {}", manifest_path.display());
    println!("wrote {}", drift_path.display());

    // prove determinism on the spot rather than trusting it
    let reread = manifest::CorpusManifest::read_json(&manifest_path)?;
    if reread.manifest_hash != m.manifest_hash {
        return Err("manifest hash changed across a write/read round trip".into());
    }
    println!("verified: manifest round-trips to the same hash");

    Ok(())
}

/// Load every timeline present under `dir`, keyed by stream id.
fn load_timelines(dir: &std::path::Path) -> Result<BTreeMap<StreamId, timeline::Timeline>, String> {
    let mut out = BTreeMap::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        // only `timeline-*.json`: the directory also holds `assets-*.json`,
        // the resolved asset tables written for the Python workers, which are
        // a different shape entirely
        let is_timeline = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("timeline-") && n.ends_with(".json"));
        if is_timeline {
            let t = timeline::Timeline::read_json(&path)?;
            out.insert(t.stream_id, t);
        }
    }
    Ok(out)
}

/// Milestone 3: commit prepared batches in short atomic transactions.
fn cmd_commit(args: &[String]) -> Result<(), String> {
    let mut prepared_dir = PathBuf::from("data/prepared");
    let mut timeline_dir = PathBuf::from("data/timeline");
    let mut db_path = PathBuf::from("data/store.db");
    let mut manifest_path = PathBuf::from("data/manifest/corpus-all-tompkins-linked.json");
    let mut stream_id: Option<StreamId> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--prepared" => { prepared_dir = PathBuf::from(&args[i + 1]); i += 2; }
            "--timelines" => { timeline_dir = PathBuf::from(&args[i + 1]); i += 2; }
            "--db" => { db_path = PathBuf::from(&args[i + 1]); i += 2; }
            "--manifest" => { manifest_path = PathBuf::from(&args[i + 1]); i += 2; }
            other => {
                stream_id = Some(other.parse().map_err(|_| "stream id must be numeric")?);
                i += 1;
            }
        }
    }
    let stream_id = stream_id.ok_or("commit needs a stream id")?;

    let timelines = load_timelines(&timeline_dir)?;
    let tl = timelines
        .get(&stream_id)
        .ok_or_else(|| format!("no timeline for stream {stream_id}; run `timeline {stream_id}` first"))?;
    println!(
        "timeline for {stream_id}: {} entries, {:.3} h, {} gaps",
        tl.entries.len(),
        tl.total_ms() as f64 / 3_600_000.0,
        tl.gap_count()
    );

    let stream_dir = prepared_dir.join(stream_id.to_string());
    let (batches, refused) = prepared::discover(&stream_dir)?;
    println!("found {} complete batches under {}", batches.len(), stream_dir.display());
    for r in &refused {
        println!("  refused {}: {}", r.what, r.reason);
    }
    if batches.is_empty() {
        return Err("no complete batches to commit".into());
    }

    let mut store = store::Store::open(&db_path);

    // the manifest's stream records go in first, so every analysis record has
    // a stream to belong to
    if let Ok(m) = manifest::CorpusManifest::read_json(&manifest_path) {
        let streams: Vec<domain::Record> = m
            .to_stream_records()
            .into_iter()
            .filter(|s| s.stream_id == stream_id)
            .map(domain::Record::Stream)
            .collect();
        if !streams.is_empty() {
            store.commit_batch(&streams);
            println!("committed {} stream record(s) from the manifest", streams.len());
        }
    }

    // Segments, so playback can resolve from the store as well as the timeline.
    //
    // Only the ones backed by a compacted asset: a partially-fetched stream has
    // a full timeline (644,749 entries for 9422) but only the fetched window is
    // playable, and committing the rest would put 601,664 records describing
    // audio nobody has into every index that touches segments. The timeline
    // JSON keeps the full mapping for when more of the stream is fetched.
    let segments: Vec<domain::Record> = tl
        .entries
        .iter()
        .filter(|e| e.compacted_asset_id.is_some())
        .map(|e| {
            domain::Record::Segment(domain::SegmentRecord {
                stream_id,
                media_sequence: e.media_sequence,
                source_object_key: e.source_object_key.clone(),
                playlist_duration_ms: timeline::ticks_to_ms(e.duration_ticks),
                source_pts_start: e.pts_start,
                source_pts_end: e.pts_end,
                cumulative_start_ms: e.cumulative_start_ms(),
                cumulative_end_ms: e.cumulative_end_ms(),
                source_etag_or_checksum: Some(e.etag.clone()).filter(|s| !s.is_empty()),
                compacted_asset_id: e.compacted_asset_id.clone(),
                asset_start_ms: e.asset_start_ticks.map(timeline::ticks_to_ms),
                is_gap: e.is_gap,
            })
        })
        .collect();
    let seg_rejected = store.commit_batch(&segments);
    println!(
        "committed {} of {} timeline entries as segment records ({} rejected); \
         the rest are not backed by compacted audio",
        segments.len() - seg_rejected.len(),
        tl.entries.len(),
        seg_rejected.len()
    );

    // one short transaction per batch, so a failure costs one asset
    let mut totals: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut all_refused: Vec<prepared::Refusal> = refused;
    let mut committed = 0usize;
    for (n, batch) in batches.iter().enumerate() {
        let (records, mut bad) = prepared::load_batch(batch, tl, stream_id);
        all_refused.append(&mut bad);
        for (k, v) in prepared::tally(&records) {
            *totals.entry(k).or_insert(0) += v;
        }
        let rejected = store.commit_batch(&records);
        committed += records.len() - rejected.len();
        for r in &rejected {
            all_refused.push(prepared::Refusal {
                what: format!("{:?}", r.key),
                reason: r.reason.clone(),
            });
        }
        if (n + 1) % 10 == 0 || n + 1 == batches.len() {
            println!("  committed {}/{} batches ({committed} records)", n + 1, batches.len());
        }
    }
    store.checkpoint();

    println!("\n=== committed ===");
    for (track, count) in &totals {
        println!("{track:12} {count}");
    }
    println!("records in store   {}", store.record_count());
    println!("coverage for {stream_id}: {:?}", store.coverage(stream_id));

    let species = store.species_vocabulary();
    println!("\n=== species ({}) ===", species.len());
    for (name, count) in species.iter().take(30) {
        println!("  {name:38} {count}");
    }

    println!("\n=== refusals ({}) ===", all_refused.len());
    let mut by_reason: BTreeMap<String, usize> = BTreeMap::new();
    for r in &all_refused {
        // group by reason rather than printing thousands of lines
        let key = r.reason.split(':').next().unwrap_or(&r.reason).to_string();
        *by_reason.entry(key).or_insert(0) += 1;
    }
    for (reason, count) in &by_reason {
        println!("  {count:6}  {reason}");
    }

    Ok(())
}

/// Retract a stream from the store, and optionally delete its files.
fn cmd_forget(args: &[String]) -> Result<(), String> {
    let stream_id: StreamId = args
        .first()
        .ok_or("forget needs a stream id")?
        .parse()
        .map_err(|_| "stream id must be numeric")?;
    let mut db_path = PathBuf::from("data/store.db");
    let mut purge = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--db" => { db_path = PathBuf::from(&args[i + 1]); i += 2; }
            "--purge" => { purge = true; i += 1; }
            other => return Err(format!("unknown flag {other:?}")),
        }
    }

    let mut store = store::Store::open(&db_path);
    let before = store.record_count();
    let keys = store.keys_for_stream(stream_id);
    println!("stream {stream_id}: {} records in the store", keys.len());

    let removed = store.forget_stream(stream_id);
    store.checkpoint();
    println!(
        "retracted {removed} records from every index ({} -> {} in store)",
        before,
        store.record_count()
    );

    // prove it rather than assert it: nothing may survive in any index
    let leftover = store.keys_for_stream(stream_id);
    if !leftover.is_empty() {
        return Err(format!("{} records survived retraction", leftover.len()));
    }
    println!("verified: no records remain for stream {stream_id}");

    if purge {
        let targets = [
            PathBuf::from(format!("data/cache/{stream_id}")),
            PathBuf::from(format!("data/assets/{stream_id}")),
            PathBuf::from(format!("data/prepared/{stream_id}")),
        ];
        for t in &targets {
            if t.exists() {
                std::fs::remove_dir_all(t).map_err(|e| format!("{}: {e}", t.display()))?;
                println!("removed {}", t.display());
            }
        }
        for f in [
            format!("data/timeline/timeline-{stream_id}.json"),
            format!("data/timeline/assets-{stream_id}.json"),
            format!("data/segments/stream-{stream_id}-stream_4.json"),
        ] {
            let p = PathBuf::from(&f);
            if p.exists() {
                std::fs::remove_file(&p).map_err(|e| format!("{f}: {e}"))?;
                println!("removed {f}");
            }
        }
        println!(
            "\nnote: re-fetching this stream would cost its segment count in GetObject \
             calls against a bucket someone else pays for."
        );
    }
    Ok(())
}

/// Milestone 4: serve the search UI.
fn cmd_serve(args: &[String]) -> Result<(), String> {
    let mut db_path = PathBuf::from("data/store.db");
    let mut timeline_dir = PathBuf::from("data/timeline");
    let mut asset_dir = PathBuf::from("data/assets");
    let mut manifest_path = PathBuf::from("data/manifest/corpus-all-tompkins-linked.json");
    let mut clap_addr = Some("127.0.0.1:8181".to_string());
    let mut port = 3000u16;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--db" => { db_path = PathBuf::from(&args[i + 1]); i += 2; }
            "--timelines" => { timeline_dir = PathBuf::from(&args[i + 1]); i += 2; }
            "--assets" => { asset_dir = PathBuf::from(&args[i + 1]); i += 2; }
            "--manifest" => { manifest_path = PathBuf::from(&args[i + 1]); i += 2; }
            "--clap" => { clap_addr = Some(args[i + 1].clone()); i += 2; }
            "--no-clap" => { clap_addr = None; i += 1; }
            "--port" => {
                port = args[i + 1].parse().map_err(|_| "--port must be a number")?;
                i += 2;
            }
            other => return Err(format!("unknown flag {other:?}")),
        }
    }

    let timelines = load_timelines(&timeline_dir)?;
    if timelines.is_empty() {
        return Err(format!("no timelines under {}", timeline_dir.display()));
    }
    println!("loaded {} timeline(s)", timelines.len());

    let manifest = manifest::CorpusManifest::read_json(&manifest_path).ok();

    if let Some(addr) = &clap_addr {
        match search::clap_embed_text(addr, "heavy rain") {
            Ok(_) => println!("clap sidecar at {addr}: ready"),
            Err(e) => println!(
                "clap sidecar at {addr}: UNAVAILABLE ({e})\n  \
                 text-to-audio search will be skipped and reported per query.\n  \
                 start it with: pipelines/.venv/bin/python pipelines/clap_server.py"
            ),
        }
    }

    let index_html = std::fs::read_to_string("src/web/index.html")
        .or_else(|_| std::fs::read_to_string("examples/tompkins-audio-search/src/web/index.html"))
        .map_err(|e| format!("cannot read src/web/index.html: {e}"))?;

    // the master axis places every stream in the manifest on one wall clock,
    // using measured durations wherever a decoded timeline exists
    let (master, unplaceable) = match &manifest {
        Some(m) => master::MasterTimeline::build(m, &timelines),
        None => (
            master::MasterTimeline::build(
                &manifest::CorpusManifest {
                    manifest_version: 1,
                    provenance: manifest::ManifestProvenance {
                        catalog_path: String::new(),
                        catalog_blake3: String::new(),
                        links_path: String::new(),
                        links_blake3: String::new(),
                        performance_filter: String::new(),
                        selection: manifest::Selection::AllTompkinsLinked,
                    },
                    streams: Vec::new(),
                    totals: Default::default(),
                    manifest_hash: String::new(),
                },
                &timelines,
            )
            .0,
            Vec::new(),
        ),
    };
    println!(
        "master timeline: {} placements over {:.1} days from {}, {} lanes, \
         {:.2} h recorded, {:.2} h playable{}",
        master.placements.len(),
        master.total_ms as f64 / 86_400_000.0,
        master.epoch_utc,
        master.lane_count,
        master.recorded_ms as f64 / 3_600_000.0,
        master.indexed_ms as f64 / 3_600_000.0,
        if unplaceable.is_empty() {
            String::new()
        } else {
            format!(" ({} stream(s) unplaceable: {:?})", unplaceable.len(), unplaceable)
        }
    );

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let handle = server::spawn_store(
        db_path.clone(),
        timelines,
        manifest,
        clap_addr,
        PathBuf::from("data/prepared"),
        master,
        unplaceable,
        ready_tx,
    );
    let state = server::AppState {
        store: handle,
        asset_dir,
        index_html: std::sync::Arc::new(index_html),
    };

    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async move {
        match ready_rx.await {
            Ok(n) => println!("store at {}: {n} records", db_path.display()),
            Err(_) => return Err("store thread failed to open the database".to_string()),
        }
        let app = server::router(state);
        let addr = format!("0.0.0.0:{port}");
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("bind {addr}: {e}"))?;
        println!("\nlistening on http://localhost:{port}");
        axum::serve(listener, app).await.map_err(|e| e.to_string())
    })
}
