// Fetch a bounded run of source segments, read their real PTS, and remux
// them into playable assets — the Milestone 2 gate.
//
// Nothing here re-encodes: the source is AAC in MPEG-TS and the output is the
// same AAC in MP4, so the audio is bit-identical to the archive and the only
// thing that changes is the container.
//
// The request cost is stated up front and enforced: `--count` segments means
// exactly `--count` GetObject calls against the source bucket, and the script
// refuses to start if that exceeds `--budget`.
//
//   node tools/compact.mjs 9561 --from 0 --count 900 --asset-minutes 30
//   node tools/compact.mjs 9561 --from 0 --count 21759 --budget 22000

import { S3Client, GetObjectCommand, GetBucketLocationCommand } from "@aws-sdk/client-s3";
import { fromIni } from "@aws-sdk/credential-providers";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import fs from "node:fs/promises";
import path from "node:path";

const exec = promisify(execFile);

const BUCKET = "oda-production-stream-storage";
const PROFILE = process.env.ODA_PROFILE ?? "oda";
const CONCURRENCY = Number(process.env.FETCH_CONCURRENCY ?? 16);
/** 90 kHz MPEG-TS ticks, matching src/timeline.rs. */
const TICKS_PER_SECOND = 90_000;

function arg(name, dflt) {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : dflt;
}

async function makeClient() {
  const credentials = fromIni({ profile: PROFILE });
  let client = new S3Client({ region: "us-east-1", credentials });
  let region = "us-east-1";
  try {
    const loc = await client.send(new GetBucketLocationCommand({ Bucket: BUCKET }));
    region = loc.LocationConstraint || "us-east-1";
  } catch (e) {
    const hinted = e?.$response?.headers?.["x-amz-bucket-region"] ?? null;
    if (!hinted) throw e;
    region = hinted;
  }
  if (region !== "us-east-1") client = new S3Client({ region, credentials });
  return { client, region };
}

/** Fetch with a small retry: a transient 5xx should not lose an hour of work. */
async function fetchSegment(client, key, dest, attempt = 0) {
  try {
    const r = await client.send(new GetObjectCommand({ Bucket: BUCKET, Key: key }));
    const bytes = Buffer.from(await r.Body.transformToByteArray());
    await fs.writeFile(dest, bytes);
    return bytes.length;
  } catch (e) {
    if (attempt < 3 && /5\d\d|Throttl|Timeout|NetworkingError/i.test(`${e.name}${e.$metadata?.httpStatusCode ?? ""}`)) {
      await new Promise((r) => setTimeout(r, 250 * 2 ** attempt));
      return fetchSegment(client, key, dest, attempt + 1);
    }
    throw e;
  }
}

/** Run `tasks` with bounded concurrency, preserving input order in results. */
async function pooled(items, limit, fn) {
  const results = new Array(items.length);
  let next = 0;
  const workers = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (true) {
      const i = next++;
      if (i >= items.length) return;
      results[i] = await fn(items[i], i);
    }
  });
  await Promise.all(workers);
  return results;
}

/**
 * Read one segment's real timing. `start_time` and `duration` come from the
 * audio stream, in seconds, and are converted to ticks. This is what makes
 * the timeline authoritative rather than assumed.
 */
async function probePts(file) {
  const { stdout } = await exec("ffprobe", [
    "-v", "error",
    "-select_streams", "a:0",
    "-show_entries", "stream=start_time,duration,sample_rate,channels,codec_name",
    "-show_entries", "format=duration",
    "-of", "json",
    file,
  ]);
  const j = JSON.parse(stdout);
  const s = j.streams?.[0] ?? {};
  const startSec = Number(s.start_time);
  const durSec = Number(s.duration ?? j.format?.duration);
  if (!Number.isFinite(startSec) || !Number.isFinite(durSec)) {
    return { error: "ffprobe reported no usable timing" };
  }
  return {
    ptsStart: Math.round(startSec * TICKS_PER_SECOND),
    durationTicks: Math.round(durSec * TICKS_PER_SECOND),
    sampleRate: Number(s.sample_rate),
    channels: Number(s.channels),
    codec: s.codec_name,
  };
}

async function main() {
  const streamId = process.argv[2];
  if (!streamId || !/^\d+$/.test(streamId)) {
    console.error("usage: node compact.mjs <streamId> [--from N] [--count N] [--asset-minutes N] [--budget N]");
    process.exit(2);
  }
  const from = Number(arg("--from", 0));
  const count = Number(arg("--count", 900));
  const assetMinutes = Number(arg("--asset-minutes", 30));
  const budget = Number(arg("--budget", 2000));
  const rendition = process.env.ODA_RENDITION ?? "stream_4";

  if (count > budget) {
    console.error(
      `refusing to start: ${count} segments means ${count} GetObject calls against a bucket ` +
        `someone else pays for, over the --budget of ${budget}. raise it deliberately.`,
    );
    process.exit(2);
  }

  const cacheDir = process.env.SEGMENT_CACHE ?? `data/cache/${streamId}`;
  const assetDir = process.env.ASSET_DIR ?? `data/assets/${streamId}`;
  await fs.mkdir(cacheDir, { recursive: true });
  await fs.mkdir(assetDir, { recursive: true });

  const { client, region } = await makeClient();
  const numbers = Array.from({ length: count }, (_, i) => from + i);
  console.log(
    `stream ${streamId} in ${region}: segments ${from}..${from + count - 1} ` +
      `(${count} GetObject calls, budget ${budget})`,
  );

  // ---- fetch -------------------------------------------------------------
  const t0 = Date.now();
  let fetched = 0;
  let missing = [];
  let bytesTotal = 0;
  const sizes = await pooled(numbers, CONCURRENCY, async (n) => {
    const key = `${streamId}/${rendition}_${n}.ts`;
    const dest = path.join(cacheDir, `${rendition}_${n}.ts`);
    try {
      const stat = await fs.stat(dest).catch(() => null);
      if (stat && stat.size > 0) return stat.size; // resumable: already cached
      const size = await fetchSegment(client, key, dest);
      if (++fetched % 200 === 0) {
        process.stderr.write(`   fetched ${fetched}/${count}\n`);
      }
      return size;
    } catch (e) {
      missing.push({ n, key, error: `${e.name}` });
      return 0;
    }
  });
  bytesTotal = sizes.reduce((a, b) => a + b, 0);
  console.log(
    `  fetched        : ${count - missing.length}/${count} segments, ` +
      `${(bytesTotal / 1e6).toFixed(1)} MB in ${((Date.now() - t0) / 1000).toFixed(1)}s`,
  );
  if (missing.length) {
    console.log(`  ! missing      : ${missing.length} segments (become explicit timeline gaps)`);
    for (const m of missing.slice(0, 5)) console.log(`     ${m.key}: ${m.error}`);
  }

  // ---- probe -------------------------------------------------------------
  const present = numbers.filter((n) => !missing.some((m) => m.n === n));
  const t1 = Date.now();
  const probes = {};
  let probeErrors = 0;
  const probeResults = await pooled(present, CONCURRENCY, async (n, i) => {
    const file = path.join(cacheDir, `${rendition}_${n}.ts`);
    const p = await probePts(file);
    if (i > 0 && i % 500 === 0) process.stderr.write(`   probed ${i}/${present.length}\n`);
    return [n, p];
  });
  const frameHistogram = {};
  const rates = new Set();
  for (const [n, p] of probeResults) {
    if (p.error) {
      probeErrors++;
      continue;
    }
    probes[n] = { ptsStart: p.ptsStart, durationTicks: p.durationTicks };
    // 1024-sample AAC frame at 48 kHz = 1920 ticks
    const frames = p.durationTicks / 1920;
    const bucket = Number.isInteger(frames) ? String(frames) : `${frames.toFixed(3)} (non-integral)`;
    frameHistogram[bucket] = (frameHistogram[bucket] ?? 0) + 1;
    rates.add(`${p.codec} ${p.sampleRate}Hz ${p.channels}ch`);
  }
  console.log(
    `  probed         : ${Object.keys(probes).length} segments in ${((Date.now() - t1) / 1000).toFixed(1)}s` +
      (probeErrors ? `, ${probeErrors} unreadable` : ""),
  );
  console.log(`  audio          : ${[...rates].join(" | ")}`);
  console.log(`  segment length : ${JSON.stringify(frameHistogram)} (AAC frames per segment)`);

  const ptsTotal = Object.values(probes).reduce((a, p) => a + p.durationTicks, 0);
  const nominalTotal = Object.keys(probes).length * 94 * 1920;
  console.log(
    `  real duration  : ${(ptsTotal / TICKS_PER_SECOND).toFixed(3)} s vs ` +
      `${(nominalTotal / TICKS_PER_SECOND).toFixed(3)} s if every segment were 94 frames ` +
      `(drift ${((ptsTotal - nominalTotal) / TICKS_PER_SECOND).toFixed(3)} s over ${Object.keys(probes).length} segments)`,
  );

  const ptsPath = path.join(assetDir, `pts-${from}-${from + count - 1}.json`);
  await fs.writeFile(
    ptsPath,
    JSON.stringify({ streamId: Number(streamId), rendition, from, count, missing, probes }, null, 2) + "\n",
  );
  console.log(`  wrote ${ptsPath}`);

  // ---- compact -----------------------------------------------------------
  // assets break at missing media so no asset ever spans a gap
  const runs = [];
  let run = [];
  for (const n of numbers) {
    if (missing.some((m) => m.n === n) || !probes[n]) {
      if (run.length) runs.push(run);
      run = [];
      continue;
    }
    run.push(n);
  }
  if (run.length) runs.push(run);

  const targetTicks = assetMinutes * 60 * TICKS_PER_SECOND;
  const assets = [];
  let assetIndex = 0;
  let streamTicks = 0; // stream-relative position of the first segment fetched

  for (const r of runs) {
    let chunk = [];
    let chunkTicks = 0;
    const flush = async () => {
      if (!chunk.length) return;
      // The id names the segment range it covers, not a running counter.
      // A counter is not stable across runs: re-compacting with a different
      // --asset-minutes reuses `{stream}-00000` for different audio, so a
      // stale asset table from an earlier run silently collides with the
      // current one. Naming the range makes different chunkings distinct.
      const id = `${streamId}-${chunk[0]}-${chunk[chunk.length - 1]}`;
      assetIndex++;
      const listPath = path.join(assetDir, `${id}.txt`);
      await fs.writeFile(
        listPath,
        chunk.map((n) => `file '${path.resolve(cacheDir, `${rendition}_${n}.ts`)}'`).join("\n") + "\n",
      );
      const outPath = path.join(assetDir, `${id}.m4a`);
      // stream copy: same AAC bitstream, MP4 container, index at the front so
      // a browser can seek with range requests instead of downloading it all
      await exec("ffmpeg", [
        "-hide_banner", "-loglevel", "error", "-y",
        "-f", "concat", "-safe", "0", "-i", listPath,
        "-c:a", "copy",
        "-movflags", "+faststart",
        outPath,
      ]);
      const { stdout } = await exec("ffprobe", [
        "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", outPath,
      ]);
      const actualSec = Number(stdout.trim());
      const expectedSec = chunkTicks / TICKS_PER_SECOND;
      const stat = await fs.stat(outPath);
      assets.push({
        assetId: id,
        file: outPath,
        firstMediaSequence: chunk[0],
        lastMediaSequence: chunk[chunk.length - 1],
        segments: chunk.length,
        streamStartTicks: streamTicks,
        streamEndTicks: streamTicks + chunkTicks,
        expectedSeconds: expectedSec,
        actualSeconds: actualSec,
        deltaSeconds: actualSec - expectedSec,
        bytes: stat.size,
      });
      streamTicks += chunkTicks;
      await fs.unlink(listPath);
      chunk = [];
      chunkTicks = 0;
    };

    for (const n of r) {
      chunk.push(n);
      chunkTicks += probes[n].durationTicks;
      if (chunkTicks >= targetTicks) await flush();
    }
    await flush();
  }

  console.log(`\n  assets         : ${assets.length}`);
  let worst = 0;
  for (const a of assets) {
    worst = Math.max(worst, Math.abs(a.deltaSeconds));
    console.log(
      `     ${a.assetId}  seq ${a.firstMediaSequence}..${a.lastMediaSequence}  ` +
        `${a.segments} seg  ${(a.bytes / 1e6).toFixed(1)} MB  ` +
        `expected ${a.expectedSeconds.toFixed(3)}s  actual ${a.actualSeconds.toFixed(3)}s  ` +
        `delta ${a.deltaSeconds >= 0 ? "+" : ""}${a.deltaSeconds.toFixed(3)}s`,
    );
  }
  console.log(
    `  worst container/timeline disagreement: ${worst.toFixed(3)} s ` +
      `(this is the seek error a listener would feel)`,
  );

  const assetPath = path.join(assetDir, `assets-${from}-${from + count - 1}.json`);
  await fs.writeFile(
    assetPath,
    JSON.stringify({ streamId: Number(streamId), rendition, assetMinutes, assets }, null, 2) + "\n",
  );
  console.log(`  wrote ${assetPath}`);
}

main().catch((e) => {
  console.error(`fatal: ${e.name}: ${e.message}`);
  process.exit(1);
});
