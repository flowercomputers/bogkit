// Enumerate one stream's segment objects into a local segment index.
//
// Why prefix listing rather than the S3 inventory: the inventory is ORC
// (needs pyarrow), 1.1 GB compressed across 127 files, and its newest
// snapshot is 2021-12-04. A prefix-scoped list of a single stream costs
// ceil(segments / 1000) requests — 22 for a 12-hour stream, ~2,000 for the
// whole 295 East subset — which is both cheaper and resumable per stream.
// This is bounded by construction: the prefix is one stream, the expected
// object count is known from the frozen playlist's media sequence, and
// exceeding the derived page budget is a hard error rather than a slow walk.
//
// Read-only: only ListObjectsV2 is imported.
//
//   node s3-enumerate.mjs 9561 --expect 21759
//   node s3-enumerate.mjs 9561 --max-pages 40

import {
  S3Client,
  ListObjectsV2Command,
  GetBucketLocationCommand,
} from "@aws-sdk/client-s3";
import { fromIni } from "@aws-sdk/credential-providers";
import fs from "node:fs/promises";
import path from "node:path";

const BUCKET = "oda-production-stream-storage";
const PROFILE = process.env.ODA_PROFILE ?? "oda";
const RENDITION = process.env.ODA_RENDITION ?? "stream_4";

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

/** `9561/stream_4_1234.ts` -> 1234 */
function segmentNumber(key) {
  const m = key.match(/_(\d+)\.ts$/);
  return m ? Number(m[1]) : null;
}

async function enumerateStream(client, streamId, { maxPages }) {
  const prefix = `${streamId}/${RENDITION}_`;
  const segments = [];
  let token = undefined;
  let pages = 0;

  do {
    if (pages >= maxPages) {
      throw new Error(
        `stream ${streamId}: page budget ${maxPages} exhausted after ${segments.length} objects; ` +
          `raise --max-pages deliberately rather than letting a walk run long`,
      );
    }
    const r = await client.send(
      new ListObjectsV2Command({
        Bucket: BUCKET,
        Prefix: prefix,
        MaxKeys: 1000,
        ContinuationToken: token,
      }),
    );
    pages++;
    for (const c of r.Contents ?? []) {
      const n = segmentNumber(c.Key);
      if (n === null) continue; // e.g. the .m3u8 itself
      segments.push({
        n,
        key: c.Key,
        bytes: c.Size,
        etag: (c.ETag ?? "").replaceAll('"', ""),
        lastModified: c.LastModified?.toISOString(),
      });
    }
    token = r.IsTruncated ? r.NextContinuationToken : undefined;
    if (pages % 10 === 0) {
      process.stderr.write(`   ...${segments.length} objects after ${pages} pages\n`);
    }
  } while (token);

  // S3 returns keys in lexicographic order, in which stream_4_100000 precedes
  // stream_4_10001; the timeline needs numeric order
  segments.sort((a, b) => a.n - b.n);

  const gaps = [];
  for (let i = 1; i < segments.length; i++) {
    const expected = segments[i - 1].n + 1;
    if (segments[i].n !== expected) {
      gaps.push({
        afterSegment: segments[i - 1].n,
        nextSegment: segments[i].n,
        missing: segments[i].n - expected,
      });
    }
  }

  const totalBytes = segments.reduce((a, s) => a + s.bytes, 0);
  return { streamId, prefix, pages, segments, gaps, totalBytes };
}

async function main() {
  const argv = process.argv.slice(2);
  const flag = (name, dflt) => {
    const i = argv.indexOf(name);
    return i >= 0 ? Number(argv[i + 1]) : dflt;
  };
  // a flag's value is also all-digits, so skip the token after every flag
  // rather than treating "--expect 21759" as a request for stream 21759
  const flagValueAt = new Set(
    argv.flatMap((a, i) => (a.startsWith("--") ? [i + 1] : [])),
  );
  const streamIds = argv.filter(
    (a, i) => /^\d+$/.test(a) && !flagValueAt.has(i),
  );
  if (!streamIds.length) {
    console.error("usage: node s3-enumerate.mjs <streamId> [...] [--max-pages N] [--expect N]");
    process.exit(2);
  }
  const expect = flag("--expect", null);
  // default budget: enough for a 600-hour stream, still a hard ceiling
  const maxPages = flag("--max-pages", expect ? Math.ceil(expect / 1000) + 5 : 1200);

  const { client, region } = await makeClient();
  console.log(`bucket ${BUCKET} in ${region}, rendition ${RENDITION} (read-only)\n`);

  const outDir = process.env.SEGMENT_INDEX_DIR ?? "data/segments";
  await fs.mkdir(outDir, { recursive: true });

  for (const id of streamIds) {
    const t0 = Date.now();
    const r = await enumerateStream(client, id, { maxPages });
    const secs = (Date.now() - t0) / 1000;

    const first = r.segments[0]?.n;
    const last = r.segments[r.segments.length - 1]?.n;
    const missing = r.gaps.reduce((a, g) => a + g.missing, 0);
    const declaredSpan = last !== undefined ? last - first + 1 : 0;

    console.log(`stream ${id}`);
    console.log(`  objects        : ${r.segments.length} in ${r.pages} pages (${secs.toFixed(1)}s)`);
    console.log(`  numbering      : ${first}..${last} (span ${declaredSpan})`);
    console.log(`  gaps           : ${r.gaps.length}, ${missing} segments missing`);
    for (const g of r.gaps.slice(0, 10)) {
      console.log(`     after ${g.afterSegment} -> ${g.nextSegment} (${g.missing} missing)`);
    }
    if (r.gaps.length > 10) console.log(`     ... and ${r.gaps.length - 10} more gaps`);
    console.log(
      `  bytes          : ${(r.totalBytes / 1e9).toFixed(3)} GB` +
        `  (mean ${Math.round(r.totalBytes / Math.max(1, r.segments.length))} per segment)`,
    );
    // at ~2.005 s per segment this is the nominal extent; the authoritative
    // timeline still comes from decoded PTS during compaction
    console.log(
      `  nominal extent : ${((r.segments.length * 2.005333) / 3600).toFixed(3)} h ` +
        `(assuming 2.005 s segments — NOT authoritative)`,
    );
    if (expect && r.segments.length !== expect) {
      console.log(`  ! expected ${expect} objects, found ${r.segments.length}`);
    }

    const outPath = path.join(outDir, `stream-${id}-${RENDITION}.json`);
    await fs.writeFile(
      outPath,
      JSON.stringify(
        {
          streamId: Number(id),
          bucket: BUCKET,
          rendition: RENDITION,
          objectCount: r.segments.length,
          firstSegment: first,
          lastSegment: last,
          gaps: r.gaps,
          totalBytes: r.totalBytes,
          segments: r.segments,
        },
        null,
        2,
      ) + "\n",
    );
    console.log(`  wrote ${outPath}\n`);
  }
}

main().catch((e) => {
  console.error(`fatal: ${e.name}: ${e.message}`);
  process.exit(1);
});
