// Sample a stream at intervals and score whether the microphone was working.
//
// A large stretch of 9422 turned out to be a mic fault: the two channels sit at
// L/R correlation about -0.9 with over 90% of the energy below 100 Hz and
// essentially nothing in the 500-2000 Hz band where speech lives. Indexing that
// is wasted compute, and summing the channels to mono actively cancels what
// little signal remains.
//
// Finding the good stretches does not need the audio, only a taste of it: two
// segments every hour across a 359-hour stream is ~716 GetObject calls and
// ~63 MB, against 43,085 calls for a single 24-hour window. So probe first,
// fetch second.
//
//   node tools/probe-quality.mjs 9422 --every 1800 --segments 2
//   node tools/probe-quality.mjs 9422 --every 900 --from 400000 --to 500000

import { S3Client, GetObjectCommand, GetBucketLocationCommand } from "@aws-sdk/client-s3";
import { fromIni } from "@aws-sdk/credential-providers";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

const exec = promisify(execFile);
const BUCKET = "oda-production-stream-storage";
const PROFILE = process.env.ODA_PROFILE ?? "oda";
const RENDITION = process.env.ODA_RENDITION ?? "stream_4";
const SR = 16000;

const arg = (n, d) => {
  const i = process.argv.indexOf(n);
  return i >= 0 ? process.argv[i + 1] : d;
};

async function makeClient() {
  const credentials = fromIni({ profile: PROFILE });
  let client = new S3Client({ region: "us-east-1", credentials });
  let region = "us-east-1";
  try {
    const loc = await client.send(new GetBucketLocationCommand({ Bucket: BUCKET }));
    region = loc.LocationConstraint || "us-east-1";
  } catch (e) {
    const hinted = e?.$response?.headers?.["x-amz-bucket-region"];
    if (!hinted) throw e;
    region = hinted;
  }
  if (region !== "us-east-1") client = new S3Client({ region, credentials });
  return client;
}

/** Decode a run of .ts segments to interleaved stereo float32. */
async function decode(files) {
  const list = path.join(os.tmpdir(), `probe-${Date.now()}-${Math.floor(performance.now())}.txt`);
  await fs.writeFile(list, files.map((f) => `file '${f}'`).join("\n") + "\n");
  try {
    const { stdout } = await exec(
      "ffmpeg",
      ["-v", "error", "-f", "concat", "-safe", "0", "-i", list,
       "-f", "f32le", "-ac", "2", "-ar", String(SR), "-"],
      { encoding: "buffer", maxBuffer: 1 << 28 },
    );
    return new Float32Array(stdout.buffer, stdout.byteOffset, Math.floor(stdout.length / 4));
  } finally {
    await fs.unlink(list).catch(() => {});
  }
}

/**
 * Score one taste of audio.
 *
 * `corr` near -1 with the energy piled below 100 Hz is the fault signature;
 * a working mic in a park gives positive correlation and real midrange.
 */
function score(pcm) {
  const n = Math.floor(pcm.length / 2);
  if (n < SR) return null;
  const L = new Float32Array(n), R = new Float32Array(n), M = new Float32Array(n);
  for (let i = 0; i < n; i++) { L[i] = pcm[2*i]; R[i] = pcm[2*i+1]; M[i] = (L[i] + R[i]) / 2; }

  const mean = (a) => a.reduce((s, v) => s + v, 0) / a.length;
  const mL = mean(L), mR = mean(R);
  let num = 0, dL = 0, dR = 0;
  for (let i = 0; i < n; i++) {
    const a = L[i] - mL, b = R[i] - mR;
    num += a * b; dL += a * a; dR += b * b;
  }
  const corr = num / Math.sqrt(Math.max(dL * dR, 1e-30));

  // Goertzel-free band energy: a coarse DFT over a decimated window is enough
  // to tell rumble from a real soundscape
  const N = 1 << 14;
  const seg = L.subarray(0, Math.min(N, n));   // one channel: the mono sum cancels
  const bands = { lf: 0, mid: 0, hi: 0 };
  const step = 4;
  for (let k = 1; k < N / 2; k += step) {
    const f = (k * SR) / N;
    let re = 0, im = 0;
    for (let t = 0; t < seg.length; t += 8) {
      const w = 2 * Math.PI * k * t / N;
      re += seg[t] * Math.cos(w); im -= seg[t] * Math.sin(w);
    }
    const p = re * re + im * im;
    if (f < 100) bands.lf += p;
    else if (f >= 500 && f < 2000) bands.mid += p;
    else bands.hi += p;
  }
  const tot = bands.lf + bands.mid + bands.hi + 1e-20;
  const rms = (a) => 20 * Math.log10(Math.max(Math.sqrt(a.reduce((s, v) => s + v * v, 0) / a.length), 1e-12));

  return {
    corr,
    lf: bands.lf / tot,
    mid: bands.mid / tot,
    rmsL: rms(L),
    rmsMono: rms(M),
    // how much the mono downmix destroys, which is the pipeline's problem
    cancellationDb: rms(L) - rms(M),
  };
}

const isDead = (s) => s.corr < -0.3 || (s.lf > 0.8 && s.mid < 0.02);

async function main() {
  const streamId = process.argv[2];
  if (!streamId || !/^\d+$/.test(streamId)) {
    console.error("usage: probe-quality.mjs <streamId> [--every N] [--segments K] [--from N] [--to N]");
    process.exit(2);
  }
  const every = Number(arg("--every", 1800));       // ~1 h at ~2 s/segment
  const perProbe = Number(arg("--segments", 2));
  const idxPath = `data/segments/stream-${streamId}-${RENDITION}.json`;
  const index = JSON.parse(await fs.readFile(idxPath, "utf8"));
  const total = index.objectCount;
  const from = Number(arg("--from", 0));
  const to = Number(arg("--to", total));

  const points = [];
  for (let n = from; n < to; n += every) points.push(n);
  const gets = points.length * perProbe;
  console.log(`stream ${streamId}: ${points.length} probes x ${perProbe} segments = ${gets} GetObject calls`);
  console.log(`(a single 24 h window costs 43,085)\n`);

  const client = await makeClient();
  const tmp = await fs.mkdtemp(path.join(os.tmpdir(), "probe-"));
  const rows = [];

  console.log(`${"segment".padStart(8)} ${"hours".padStart(7)} ${"corr".padStart(6)} ${"<100Hz".padStart(7)} ${"500-2k".padStart(7)} ${"cancel".padStart(7)}  verdict`);
  for (const n of points) {
    const files = [];
    try {
      for (let k = 0; k < perProbe; k++) {
        const key = `${streamId}/${RENDITION}_${n + k}.ts`;
        const r = await client.send(new GetObjectCommand({ Bucket: BUCKET, Key: key }));
        const dest = path.join(tmp, `${n + k}.ts`);
        await fs.writeFile(dest, Buffer.from(await r.Body.transformToByteArray()));
        files.push(dest);
      }
      const s = score(await decode(files));
      if (!s) continue;
      const dead = isDead(s);
      rows.push({ segment: n, hours: (n * 1.989) / 3600, ...s, dead });
      console.log(
        `${String(n).padStart(8)} ${((n * 1.989) / 3600).toFixed(1).padStart(7)} ` +
        `${s.corr.toFixed(3).padStart(6)} ${(100 * s.lf).toFixed(1).padStart(6)}% ` +
        `${(100 * s.mid).toFixed(2).padStart(6)}% ${s.cancellationDb.toFixed(1).padStart(6)}dB  ` +
        (dead ? "dead" : "OK"),
      );
    } catch (e) {
      console.log(`${String(n).padStart(8)} ${" ".repeat(30)} error: ${e.name}`);
    } finally {
      for (const f of files) await fs.unlink(f).catch(() => {});
    }
  }
  await fs.rm(tmp, { recursive: true, force: true });

  // longest contiguous run of healthy probes
  let best = { start: null, end: null, len: 0 }, cur = null;
  for (const r of rows) {
    if (!r.dead) {
      cur = cur ?? { start: r.segment, end: r.segment, len: 0 };
      cur.end = r.segment; cur.len++;
      if (cur.len > best.len) best = { ...cur };
    } else cur = null;
  }
  const okCount = rows.filter((r) => !r.dead).length;
  console.log(`\n${okCount}/${rows.length} probes healthy`);
  if (best.len > 1) {
    const hrs = ((best.end - best.start) * 1.989) / 3600;
    console.log(
      `longest healthy run: segments ${best.start}..${best.end} ` +
      `(~${hrs.toFixed(1)} h, ${best.len} consecutive probes)`,
    );
  } else {
    console.log("no run of two consecutive healthy probes found");
  }

  const out = arg("--out", `data/probe/quality-${streamId}.json`);
  await fs.mkdir(path.dirname(out), { recursive: true });
  await fs.writeFile(out, JSON.stringify({ streamId: Number(streamId), every, perProbe, rows }, null, 2) + "\n");
  console.log(`wrote ${out}`);
}

main().catch((e) => { console.error(`fatal: ${e.name}: ${e.message}`); process.exit(1); });
