// Bounded, read-only reconnaissance of the Oda archive.
//
// Safety rules this script enforces structurally rather than by convention:
//
//   * only GetObject / HeadObject / ListObjectsV2 / GetBucketLocation are
//     imported, so there is no code path that can write, tag, copy or delete;
//   * ListObjectsV2 is called with MaxKeys and never paginated, so there is
//     no unbounded walk of a bucket holding millions of objects;
//   * the request budget is counted and printed, because the source bucket's
//     owner pays for every one of them.
//
// The Intel build of the aws CLI at /usr/local/aws-cli hangs on Apple
// Silicon, which is why this uses the JS SDK with the shared-credentials
// provider instead.
//
//   node s3-probe.mjs 9258 9561 9606 9225

import {
  S3Client,
  GetObjectCommand,
  HeadObjectCommand,
  ListObjectsV2Command,
  GetBucketLocationCommand,
} from "@aws-sdk/client-s3";
import { fromIni } from "@aws-sdk/credential-providers";

const BUCKET = "oda-production-stream-storage";
const PROFILE = process.env.ODA_PROFILE ?? "oda";
/** Hard ceiling on source-bucket requests for one run of this script. */
const REQUEST_BUDGET = 200;

let requests = 0;
function spend(what) {
  if (++requests > REQUEST_BUDGET) {
    throw new Error(
      `request budget of ${REQUEST_BUDGET} exhausted at ${what}; refusing to keep reading a bucket someone else pays for`,
    );
  }
}

async function makeClient() {
  const credentials = fromIni({ profile: PROFILE });
  // us-east-1 is the safe probe region: S3 answers GetBucketLocation from
  // anywhere and tells us where the bucket actually lives.
  let client = new S3Client({ region: "us-east-1", credentials });
  let region = "us-east-1";
  try {
    spend("GetBucketLocation");
    const loc = await client.send(
      new GetBucketLocationCommand({ Bucket: BUCKET }),
    );
    region = loc.LocationConstraint || "us-east-1";
  } catch (e) {
    // a redirect carries the true region in a header even when the call fails
    const hinted =
      e?.$response?.headers?.["x-amz-bucket-region"] ?? e?.Region ?? null;
    if (!hinted) throw e;
    region = hinted;
  }
  if (region !== "us-east-1") {
    client = new S3Client({ region, credentials });
  }
  return { client, region };
}

async function getText(client, key) {
  spend(`GetObject ${key}`);
  const r = await client.send(
    new GetObjectCommand({ Bucket: BUCKET, Key: key }),
  );
  return await r.Body.transformToString();
}

async function head(client, key) {
  spend(`HeadObject ${key}`);
  return await client.send(
    new HeadObjectCommand({ Bucket: BUCKET, Key: key }),
  );
}

async function listOnePage(client, prefix, maxKeys = 40, delimiter = undefined) {
  spend(`ListObjectsV2 ${prefix}`);
  const r = await client.send(
    new ListObjectsV2Command({
      Bucket: BUCKET,
      Prefix: prefix,
      MaxKeys: maxKeys,
      Delimiter: delimiter,
    }),
  );
  return r;
}

/** Parse an HLS playlist into tags and URIs, without interpreting them yet. */
function parsePlaylist(text) {
  const lines = text.split(/\r?\n/);
  const variants = []; // master: { attrs, uri }
  const segments = []; // media: { durationSec, uri, discontinuityBefore }
  let mediaSequence = null;
  let targetDuration = null;
  let hasEndList = false;
  let programDateTimes = [];
  let pendingDiscontinuity = false;
  let pendingExtinf = null;
  let pendingStreamInf = null;

  for (const raw of lines) {
    const line = raw.trim();
    if (!line) continue;
    if (line.startsWith("#EXT-X-MEDIA-SEQUENCE:")) {
      mediaSequence = Number(line.slice(22));
    } else if (line.startsWith("#EXT-X-TARGETDURATION:")) {
      targetDuration = Number(line.slice(22));
    } else if (line === "#EXT-X-ENDLIST") {
      hasEndList = true;
    } else if (line.startsWith("#EXT-X-PROGRAM-DATE-TIME:")) {
      programDateTimes.push(line.slice(25));
    } else if (line === "#EXT-X-DISCONTINUITY") {
      pendingDiscontinuity = true;
    } else if (line.startsWith("#EXTINF:")) {
      pendingExtinf = Number(line.slice(8).split(",")[0]);
    } else if (line.startsWith("#EXT-X-STREAM-INF:")) {
      pendingStreamInf = line.slice(18);
    } else if (!line.startsWith("#")) {
      if (pendingStreamInf !== null) {
        variants.push({ attrs: pendingStreamInf, uri: line });
        pendingStreamInf = null;
      } else {
        segments.push({
          durationSec: pendingExtinf,
          uri: line,
          discontinuityBefore: pendingDiscontinuity,
        });
        pendingExtinf = null;
        pendingDiscontinuity = false;
      }
    }
  }
  return {
    variants,
    segments,
    mediaSequence,
    targetDuration,
    hasEndList,
    programDateTimes,
  };
}

/** Segment index embedded in a media URI, for detecting numbering gaps. */
function segmentIndex(uri) {
  const m = uri.match(/(\d+)(?=\.(ts|aac|m4s|mp4)\b)|(\d+)$/i);
  return m ? Number(m[1] ?? m[3]) : null;
}

function summarizeDurations(segments) {
  const ds = segments.map((s) => s.durationSec).filter((d) => Number.isFinite(d));
  if (!ds.length) return null;
  const sum = ds.reduce((a, b) => a + b, 0);
  const sorted = [...ds].sort((a, b) => a - b);
  return {
    count: ds.length,
    totalSec: sum,
    min: sorted[0],
    median: sorted[Math.floor(sorted.length / 2)],
    max: sorted[sorted.length - 1],
    distinct: new Set(ds.map((d) => d.toFixed(3))).size,
  };
}

/** Numbering gaps: missing media that must become explicit timeline gaps. */
function findNumberingGaps(segments) {
  const idx = segments.map((s) => segmentIndex(s.uri)).filter((n) => n !== null);
  const gaps = [];
  for (let i = 1; i < idx.length; i++) {
    if (idx[i] !== idx[i - 1] + 1) {
      gaps.push({ after: idx[i - 1], next: idx[i], missing: idx[i] - idx[i - 1] - 1 });
    }
  }
  return { first: idx[0] ?? null, last: idx[idx.length - 1] ?? null, gaps };
}

async function probeStream(client, streamId) {
  const out = { streamId, errors: [] };
  console.log(`\n${"=".repeat(72)}\nstream ${streamId}\n${"=".repeat(72)}`);

  // which rendition objects actually exist? the master advertises five, but
  // only the top one was present on earlier inspection
  try {
    const top = await listOnePage(client, `${streamId}/`, 20, "/");
    const prefixes = (top.CommonPrefixes ?? []).map((p) => p.Prefix);
    const files = (top.Contents ?? []).map((c) => c.Key);
    out.childPrefixes = prefixes;
    out.rootObjects = files;
    console.log(`  child prefixes : ${prefixes.join(", ") || "(none)"}`);
    console.log(`  root objects   : ${files.join(", ") || "(none)"}`);
  } catch (e) {
    out.errors.push(`list ${streamId}/: ${e.name}: ${e.message}`);
    console.log(`  ! list failed: ${e.name}: ${e.message}`);
  }

  // master playlist
  let master = null;
  for (const key of [`${streamId}/stream.m3u8`, `${streamId}/master.m3u8`]) {
    try {
      const text = await getText(client, key);
      master = { key, ...parsePlaylist(text) };
      break;
    } catch (e) {
      out.errors.push(`get ${key}: ${e.name}`);
    }
  }
  if (!master) {
    console.log("  ! no master playlist found");
    return out;
  }
  out.masterKey = master.key;
  console.log(`  master         : ${master.key}`);
  console.log(`  renditions     : ${master.variants.length} advertised`);
  for (const v of master.variants) {
    console.log(`     ${v.uri.padEnd(24)} ${v.attrs}`);
  }

  // pick the highest-bandwidth rendition whose media objects are present
  const ranked = [...master.variants].sort((a, b) => {
    const bw = (s) => Number(/BANDWIDTH=(\d+)/.exec(s.attrs)?.[1] ?? 0);
    return bw(b) - bw(a);
  });

  for (const variant of ranked) {
    const mediaKey = variant.uri.startsWith("http")
      ? new URL(variant.uri).pathname.replace(/^\//, "")
      : `${streamId}/${variant.uri}`;
    let media;
    try {
      media = parsePlaylist(await getText(client, mediaKey));
    } catch (e) {
      console.log(`  rendition ${variant.uri}: playlist missing (${e.name})`);
      continue;
    }
    const durations = summarizeDurations(media.segments);
    const numbering = findNumberingGaps(media.segments);

    // confirm the media objects themselves exist, not just the playlist
    const probeUris = [
      media.segments[0],
      media.segments[Math.floor(media.segments.length / 2)],
      media.segments[media.segments.length - 1],
    ].filter(Boolean);
    const present = [];
    for (const seg of probeUris) {
      const segKey = seg.uri.startsWith("http")
        ? new URL(seg.uri).pathname.replace(/^\//, "")
        : `${mediaKey.split("/").slice(0, -1).join("/")}/${seg.uri}`;
      try {
        const h = await head(client, segKey);
        present.push({
          key: segKey,
          bytes: h.ContentLength,
          etag: h.ETag,
          lastModified: h.LastModified?.toISOString(),
          durationSec: seg.durationSec,
        });
      } catch (e) {
        present.push({ key: segKey, error: `${e.name}` });
      }
    }
    const ok = present.filter((p) => !p.error);

    console.log(`\n  --- rendition ${variant.uri} ---`);
    console.log(`  media playlist : ${mediaKey}`);
    console.log(`  segments       : ${media.segments.length}`);
    console.log(`  media sequence : ${media.mediaSequence}`);
    console.log(`  target dur     : ${media.targetDuration}`);
    console.log(`  ENDLIST        : ${media.hasEndList}`);
    console.log(
      `  PROGRAM-DATE-TIME: ${media.programDateTimes.length} tags` +
        (media.programDateTimes.length
          ? ` (first ${media.programDateTimes[0]})`
          : "  <-- no wall clock recoverable from HLS"),
    );
    if (durations) {
      console.log(
        `  EXTINF         : total ${(durations.totalSec / 3600).toFixed(3)} h, ` +
          `min ${durations.min}, median ${durations.median}, max ${durations.max}, ` +
          `${durations.distinct} distinct values`,
      );
    }
    console.log(
      `  discontinuities: ${media.segments.filter((s) => s.discontinuityBefore).length}`,
    );
    console.log(
      `  numbering      : ${numbering.first}..${numbering.last}, ` +
        `${numbering.gaps.length} gap(s)` +
        (numbering.gaps.length
          ? ` missing ${numbering.gaps.reduce((a, g) => a + g.missing, 0)} segments`
          : ""),
    );
    for (const g of numbering.gaps.slice(0, 5)) {
      console.log(`     gap after ${g.after} -> ${g.next} (${g.missing} missing)`);
    }
    for (const p of present) {
      console.log(
        `  probe          : ${p.key} ` +
          (p.error
            ? `MISSING (${p.error})`
            : `${p.bytes} bytes, ${p.durationSec}s, modified ${p.lastModified}`),
      );
    }
    if (ok.length) {
      const bps =
        (ok.reduce((a, p) => a + p.bytes, 0) * 8) /
        ok.reduce((a, p) => a + (p.durationSec ?? 0), 0);
      console.log(`  effective rate : ${(bps / 1000).toFixed(1)} kbps (from ${ok.length} probes)`);
    }

    out.selected = {
      rendition: variant.uri,
      mediaKey,
      segmentCount: media.segments.length,
      mediaSequence: media.mediaSequence,
      targetDuration: media.targetDuration,
      hasEndList: media.hasEndList,
      programDateTimeCount: media.programDateTimes.length,
      durations,
      discontinuityCount: media.segments.filter((s) => s.discontinuityBefore).length,
      numbering,
      probes: present,
      mediaPresent: ok.length,
    };
    // first rendition with real media wins; do not probe the rest
    if (ok.length) break;
  }
  return out;
}

async function main() {
  const streamIds = process.argv.slice(2);
  if (!streamIds.length) {
    console.error("usage: node s3-probe.mjs <streamId> [streamId ...]");
    process.exit(2);
  }

  const { client, region } = await makeClient();
  console.log(`bucket ${BUCKET} in ${region}, profile "${PROFILE}" (read-only)`);

  const results = [];
  for (const id of streamIds) {
    try {
      results.push(await probeStream(client, id));
    } catch (e) {
      console.log(`\n! stream ${id} aborted: ${e.message}`);
      results.push({ streamId: id, fatal: e.message });
      if (/request budget/.test(e.message)) break;
    }
  }

  console.log(`\n${"=".repeat(72)}`);
  console.log(`source-bucket requests spent: ${requests} / ${REQUEST_BUDGET}`);

  const outPath = process.env.PROBE_OUT ?? "data/probe/s3-probe.json";
  const fs = await import("node:fs/promises");
  const path = await import("node:path");
  await fs.mkdir(path.dirname(outPath), { recursive: true });
  await fs.writeFile(
    outPath,
    JSON.stringify({ bucket: BUCKET, region, requests, results }, null, 2) + "\n",
  );
  console.log(`wrote ${outPath}`);
}

main().catch((e) => {
  console.error(`fatal: ${e.name}: ${e.message}`);
  process.exit(1);
});
