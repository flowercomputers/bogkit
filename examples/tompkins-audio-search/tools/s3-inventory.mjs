// Locate and describe the daily S3 inventory for the archive.
//
// The inventory is the sanctioned way to enumerate objects: the production
// bucket holds millions of ~2-second segments, so a ListObjectsV2 walk would
// cost the bucket owner thousands of requests per stream and take hours. The
// inventory delivers the same object list as a handful of compressed files.
//
// This script only *describes* the inventory — which partitions exist, what
// schema and format the newest manifest declares, how large the data files
// are. Fetching and filtering it is a separate, explicitly-budgeted step.
//
//   node s3-inventory.mjs

import {
  S3Client,
  GetObjectCommand,
  ListObjectsV2Command,
  GetBucketLocationCommand,
} from "@aws-sdk/client-s3";
import { fromIni } from "@aws-sdk/credential-providers";

const INVENTORY_BUCKET = "stream-inventory";
const INVENTORY_PREFIX = "oda-production-stream-storage/stream-index/";
const PROFILE = process.env.ODA_PROFILE ?? "oda";
const REQUEST_BUDGET = 60;

let requests = 0;
function spend(what) {
  if (++requests > REQUEST_BUDGET) {
    throw new Error(`request budget of ${REQUEST_BUDGET} exhausted at ${what}`);
  }
}

async function makeClient(bucket) {
  const credentials = fromIni({ profile: PROFILE });
  let client = new S3Client({ region: "us-east-1", credentials });
  let region = "us-east-1";
  try {
    spend("GetBucketLocation");
    const loc = await client.send(new GetBucketLocationCommand({ Bucket: bucket }));
    region = loc.LocationConstraint || "us-east-1";
  } catch (e) {
    const hinted = e?.$response?.headers?.["x-amz-bucket-region"] ?? null;
    if (!hinted) throw e;
    region = hinted;
  }
  if (region !== "us-east-1") client = new S3Client({ region, credentials });
  return { client, region };
}

async function listPage(client, bucket, prefix, delimiter, maxKeys = 100, token) {
  spend(`ListObjectsV2 ${prefix}`);
  return await client.send(
    new ListObjectsV2Command({
      Bucket: bucket,
      Prefix: prefix,
      Delimiter: delimiter,
      MaxKeys: maxKeys,
      ContinuationToken: token,
    }),
  );
}

async function getText(client, bucket, key) {
  spend(`GetObject ${key}`);
  const r = await client.send(new GetObjectCommand({ Bucket: bucket, Key: key }));
  return await r.Body.transformToString();
}

async function main() {
  const { client, region } = await makeClient(INVENTORY_BUCKET);
  console.log(`inventory bucket ${INVENTORY_BUCKET} in ${region} (read-only)\n`);

  // top level: usually one prefix per snapshot date, plus hive/ and data/
  const top = await listPage(client, INVENTORY_BUCKET, INVENTORY_PREFIX, "/");
  const prefixes = (top.CommonPrefixes ?? []).map((p) => p.Prefix);
  const objects = (top.Contents ?? []).map((c) => c.Key);
  console.log(`child prefixes (${prefixes.length}):`);
  for (const p of prefixes.slice(0, 20)) console.log(`   ${p}`);
  if (prefixes.length > 20) console.log(`   ... and ${prefixes.length - 20} more`);
  console.log(`\nobjects at this level (${objects.length}):`);
  for (const o of objects.slice(0, 10)) console.log(`   ${o}`);

  // the newest date partition, by lexicographic order (ISO dates sort right)
  const datePrefixes = prefixes.filter((p) => /\d{4}-\d{2}-\d{2}/.test(p)).sort();
  const newest = datePrefixes[datePrefixes.length - 1];
  if (!newest) {
    console.log("\nno date-partitioned snapshots found under this prefix");
    console.log(`requests spent: ${requests}`);
    return;
  }
  console.log(`\nnewest snapshot: ${newest}`);

  const snap = await listPage(client, INVENTORY_BUCKET, newest, undefined, 50);
  const snapObjects = (snap.Contents ?? []).map((c) => ({
    key: c.Key,
    bytes: c.Size,
  }));
  console.log(`snapshot objects (${snapObjects.length}${snap.IsTruncated ? "+" : ""}):`);
  for (const o of snapObjects) console.log(`   ${o.key}  ${o.bytes} bytes`);

  const manifestKey = snapObjects.find((o) => o.key.endsWith("manifest.json"))?.key;
  let manifest = null;
  if (manifestKey) {
    manifest = JSON.parse(await getText(client, INVENTORY_BUCKET, manifestKey));
    console.log(`\nmanifest ${manifestKey}:`);
    console.log(`   sourceBucket : ${manifest.sourceBucket}`);
    console.log(`   format       : ${manifest.fileFormat}`);
    console.log(`   schema       : ${manifest.fileSchema}`);
    console.log(`   files        : ${(manifest.files ?? []).length}`);
    const totalBytes = (manifest.files ?? []).reduce((a, f) => a + (f.size ?? 0), 0);
    console.log(`   total size   : ${(totalBytes / 1e9).toFixed(3)} GB compressed`);
    for (const f of (manifest.files ?? []).slice(0, 5)) {
      console.log(`      ${f.key}  ${f.size} bytes  ${f.MD5checksum ?? ""}`);
    }
    if ((manifest.files ?? []).length > 5) {
      console.log(`      ... and ${manifest.files.length - 5} more`);
    }
  } else {
    console.log("\nno manifest.json in the newest snapshot");
  }

  console.log(`\nrequests spent: ${requests} / ${REQUEST_BUDGET}`);

  const fs = await import("node:fs/promises");
  const path = await import("node:path");
  const outPath = process.env.INVENTORY_OUT ?? "data/probe/s3-inventory.json";
  await fs.mkdir(path.dirname(outPath), { recursive: true });
  await fs.writeFile(
    outPath,
    JSON.stringify(
      { bucket: INVENTORY_BUCKET, region, prefixes, newest, snapObjects, manifest },
      null,
      2,
    ) + "\n",
  );
  console.log(`wrote ${outPath}`);
}

main().catch((e) => {
  console.error(`fatal: ${e.name}: ${e.message}`);
  process.exit(1);
});
