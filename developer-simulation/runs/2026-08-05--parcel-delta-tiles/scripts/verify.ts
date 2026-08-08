#!/usr/bin/env node
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

type Position = [number, number];
type Geometry = { type: "Polygon" | "MultiPolygon"; coordinates: unknown };
type Edit = { id: string; old?: Geometry | null; new?: Geometry | null };

const binary = resolve(process.argv[2] ?? "target/release/parcel-delta-tiles");
const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const reference = join(root, "scripts/reference.ts");
const work = mkdtempSync(join(tmpdir(), "parcel-delta-verify-"));

function rectangle(minX: number, minY: number, maxX: number, maxY: number): Geometry {
  return {
    type: "Polygon",
    coordinates: [[
      [minX, minY],
      [maxX, minY],
      [maxX, maxY],
      [minX, maxY],
      [minX, minY],
    ]],
  };
}

function ndjson(edits: Edit[]): string {
  return `${edits.map((edit) => JSON.stringify(edit)).join("\n")}\n`;
}

function run(command: string, args: string[]) {
  return spawnSync(command, args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
}

function compare(label: string, edits: Edit[]): string {
  const path = join(work, `${label}.ndjson`);
  writeFileSync(path, ndjson(edits));
  const actual = run(binary, [path]);
  const expected = run(process.execPath, [reference, path]);
  if (actual.status !== 0) throw new Error(`${label}: Rust failed: ${actual.stderr}`);
  if (expected.status !== 0) throw new Error(`${label}: reference failed: ${expected.stderr}`);
  if (actual.stdout !== expected.stdout) {
    const actualLines = actual.stdout.trim().split("\n");
    const expectedLines = expected.stdout.trim().split("\n");
    throw new Error(
      `${label}: mismatch; Rust=${actualLines.length} tiles reference=${expectedLines.length} tiles`,
    );
  }
  const count = actual.stdout === "" ? 0 : actual.stdout.trim().split("\n").length;
  console.log(`mirror ${label}: exact (${count} tiles)`);
  return actual.stdout;
}

function tileLongitude(x: number, z: number): number {
  return (x / 2 ** z) * 360 - 180;
}

function tileLatitude(y: number, z: number): number {
  return (Math.atan(Math.sinh(Math.PI * (1 - (2 * y) / 2 ** z))) * 180) / Math.PI;
}

function seeded(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 2 ** 32;
  };
}

function seededEdits(count: number, seed: number): Edit[] {
  const random = seeded(seed);
  const edits: Edit[] = [];
  for (let index = 0; index < count; index += 1) {
    const x = -73.991 + random() * 0.008;
    const y = 40.721 + random() * 0.008;
    const width = 0.00003 + random() * 0.00008;
    const height = 0.00003 + random() * 0.00008;
    const shape = rectangle(x, y, x + width, y + height);
    if (index % 3 === 0) edits.push({ id: `seed-${index}`, new: shape });
    else if (index % 3 === 1) edits.push({ id: `seed-${index}`, old: shape });
    else {
      edits.push({
        id: `seed-${index}`,
        old: shape,
        new: rectangle(x + 0.00002, y - 0.00001, x + width + 0.00002, y + height - 0.00001),
      });
    }
  }
  return edits;
}

function shuffle<T>(input: T[], seed: number): T[] {
  const output = [...input];
  const random = seeded(seed);
  for (let index = output.length - 1; index > 0; index -= 1) {
    const other = Math.floor(random() * (index + 1));
    [output[index], output[other]] = [output[other], output[index]];
  }
  return output;
}

function requireMalformed(label: string, fixture: string, line: number, fragment: string): void {
  requireMalformedText(label, readFileSync(join(root, fixture), "utf8"), line, fragment);
}

function requireMalformedText(label: string, contents: string, line: number, fragment: string): void {
  const path = join(work, `malformed-${label}.ndjson`);
  writeFileSync(path, contents.endsWith("\n") ? contents : `${contents}\n`);
  const first = run(binary, [path]);
  const second = run(binary, [path]);
  if (first.status === 0 || first.stdout !== "") {
    throw new Error(`${label}: expected nonzero status and empty stdout`);
  }
  if (!first.stderr.includes(`input line ${line}:`) || !first.stderr.includes(fragment)) {
    throw new Error(`${label}: unexpected diagnostic: ${first.stderr}`);
  }
  if (first.status !== second.status || first.stdout !== second.stdout || first.stderr !== second.stderr) {
    throw new Error(`${label}: failure was not deterministic`);
  }
  console.log(`malformed ${label}: deterministic line ${line}, empty stdout`);
}

function rectangleTileSet(edits: Edit[]): string {
  const tiles = new Set<string>();
  for (const edit of edits) {
    for (const geometry of [edit.old, edit.new]) {
      if (!geometry || geometry.type !== "Polygon") continue;
      const ring = geometry.coordinates as Position[][];
      const xs = ring[0].map(([x]) => x);
      const ys = ring[0].map(([, y]) => y);
      const minX = Math.min(...xs);
      const maxX = Math.max(...xs);
      const minY = Math.min(...ys);
      const maxY = Math.max(...ys);
      for (let z = 12; z <= 16; z += 1) {
        const n = 2 ** z;
        const xWorld = (longitude: number) => ((longitude + 180) / 360) * n;
        const yWorld = (latitude: number) => {
          const radians = (latitude * Math.PI) / 180;
          return ((1 - Math.asinh(Math.tan(radians)) / Math.PI) / 2) * n;
        };
        const touching = (min: number, max: number): [number, number] => {
          const near = (value: number) => Math.abs(value - Math.round(value)) <= 1e-10;
          const lower = near(min) ? Math.round(min) - 1 : Math.floor(min);
          const upper = near(max) ? Math.round(max) : Math.floor(max);
          return [Math.max(0, lower), Math.min(n - 1, upper)];
        };
        const [xStart, xEnd] = touching(xWorld(minX), xWorld(maxX));
        const yA = yWorld(minY);
        const yB = yWorld(maxY);
        const [yStart, yEnd] = touching(Math.min(yA, yB), Math.max(yA, yB));
        for (let x = xStart; x <= xEnd; x += 1) {
          for (let y = yStart; y <= yEnd; y += 1) tiles.add(`${z}/${x}/${y}`);
        }
      }
    }
  }
  const sorted = [...tiles].sort();
  return sorted.length === 0 ? "" : `${sorted.join("\n")}\n`;
}

function verifyAnalyticalRectangles(): void {
  const random = seeded(0xdecafbad);
  const edits: Edit[] = [];
  for (let index = 0; index < 500; index += 1) {
    const x = -170 + random() * 340;
    const y = -70 + random() * 140;
    const width = 0.001 + random() * 0.05;
    const height = 0.001 + random() * 0.05;
    edits.push({ id: `analytic-${index}`, new: rectangle(x, y, x + width, y + height) });
  }
  const path = join(work, "analytic-rectangles.ndjson");
  writeFileSync(path, ndjson(edits));
  const actual = run(binary, [path]);
  if (actual.status !== 0) throw new Error(`analytical rectangles failed: ${actual.stderr}`);
  const expected = rectangleTileSet(edits);
  if (actual.stdout !== expected) throw new Error("analytical rectangle tile set differs");
  console.log(`analytical rectangles: 500 edits exact (${expected.trim().split("\n").length} tiles)`);
}

try {
  const insertion = { id: "insertion", new: rectangle(-73.99, 40.72, -73.98, 40.73) };
  compare("insertion", [insertion]);
  compare("deletion", [{ id: "deletion", old: insertion.new }]);
  compare("translation", [{
    id: "translation",
    old: rectangle(-73.99, 40.72, -73.985, 40.725),
    new: rectangle(-73.98, 40.73, -73.975, 40.735),
  }]);
  compare("concavity", [{
    id: "concavity",
    new: {
      type: "Polygon",
      coordinates: [[
        [-74.00, 40.71], [-73.98, 40.71], [-73.98, 40.72], [-73.99, 40.72],
        [-73.99, 40.73], [-74.00, 40.73], [-74.00, 40.71],
      ]],
    },
  }]);

  const holeTile = { z: 16, x: 19301, y: 24640 };
  const left = tileLongitude(holeTile.x, holeTile.z);
  const right = tileLongitude(holeTile.x + 1, holeTile.z);
  const top = tileLatitude(holeTile.y, holeTile.z);
  const bottom = tileLatitude(holeTile.y + 1, holeTile.z);
  const dx = right - left;
  const dy = top - bottom;
  const hole: Position[] = [
    [left - dx / 4, bottom - dy / 4], [right + dx / 4, bottom - dy / 4],
    [right + dx / 4, top + dy / 4], [left - dx / 4, top + dy / 4],
    [left - dx / 4, bottom - dy / 4],
  ];
  const outer: Position[] = [
    [left - dx * 2, bottom - dy * 2], [right + dx * 2, bottom - dy * 2],
    [right + dx * 2, top + dy * 2], [left - dx * 2, top + dy * 2],
    [left - dx * 2, bottom - dy * 2],
  ];
  const holeOutput = compare("holes", [{
    id: "holes",
    new: { type: "Polygon", coordinates: [outer, hole] },
  }]);
  if (holeOutput.split("\n").includes(`${holeTile.z}/${holeTile.x}/${holeTile.y}`)) {
    throw new Error("holes: tile wholly inside the hole was included");
  }

  compare("multipolygon", [{
    id: "multipolygon",
    new: {
      type: "MultiPolygon",
      coordinates: [
        rectangle(-73.995, 40.715, -73.993, 40.717).coordinates,
        rectangle(-73.975, 40.732, -73.972, 40.734).coordinates,
      ],
    },
  }]);

  const boundary = tileLongitude(19301, 16);
  const boundaryOutput = compare("boundary-touch", [{
    id: "boundary-touch",
    new: rectangle(boundary, 40.72, boundary + 0.0002, 40.721),
  }]);
  const boundaryXs = new Set(
    boundaryOutput.trim().split("\n").filter((tile) => tile.startsWith("16/")).map((tile) => tile.split("/")[1]),
  );
  if (!boundaryXs.has("19300") || !boundaryXs.has("19301")) {
    throw new Error("boundary-touch: did not include both tiles sharing the touched edge");
  }

  const tenThousand = seededEdits(10_000, 0x5eed1234);
  compare("seeded-10000", tenThousand);
  verifyAnalyticalRectangles();

  const permutationEdits = seededEdits(200, 0xc0ffee);
  const basePath = join(work, "permutation-base.ndjson");
  writeFileSync(basePath, ndjson(permutationEdits));
  const base = run(binary, [basePath]);
  if (base.status !== 0) throw new Error(`permutation base failed: ${base.stderr}`);
  for (let index = 0; index < 10; index += 1) {
    const path = join(work, `permutation-${index}.ndjson`);
    writeFileSync(path, ndjson(shuffle(permutationEdits, 1000 + index)));
    const result = run(binary, [path]);
    if (result.status !== 0 || result.stdout !== base.stdout) {
      throw new Error(`permutation ${index + 1}: output differs`);
    }
  }
  console.log("permutations: 10/10 byte-identical");

  requireMalformed("open-ring", "fixtures/malformed-open-ring.ndjson", 2, "is open");
  requireMalformed("coordinate", "fixtures/malformed-coordinate.ndjson", 1, "longitude");
  requireMalformed("nonfinite", "fixtures/malformed-nonfinite.ndjson", 1, "number out of range");
  requireMalformed("unsupported-type", "fixtures/malformed-type.ndjson", 1, "unsupported");
  const valid = ndjson([{ id: "valid", new: rectangle(-73.99, 40.72, -73.98, 40.73) }]);
  requireMalformedText(
    "duplicate-edit",
    `${valid.trim()}\n{"id":"dup","new":null,"new":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}}\n`,
    2,
    "duplicate object member",
  );
  requireMalformedText(
    "duplicate-geometry",
    '{"id":"dup","new":{"type":"LineString","type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}}\n',
    1,
    "duplicate object member",
  );
  requireMalformedText(
    "duplicate-nested",
    '{"id":"dup","meta":{"value":1,"value":2},"new":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}}\n',
    1,
    "duplicate object member",
  );
  requireMalformedText(
    "self-intersection",
    '{"id":"bow","new":{"type":"Polygon","coordinates":[[[0,0],[2,2],[0,2],[2,0],[0,0]]]}}\n',
    1,
    "self-intersects",
  );
  requireMalformedText(
    "zero-area",
    '{"id":"zero","new":{"type":"Polygon","coordinates":[[[0,0],[1,1],[2,2],[0,0]]]}}\n',
    1,
    "zero area",
  );
  requireMalformedText(
    "hole-outside",
    '{"id":"hole","new":{"type":"Polygon","coordinates":[[[0,0],[2,0],[2,2],[0,2],[0,0]],[[3,3],[4,3],[4,4],[3,4],[3,3]]]}}\n',
    1,
    "strictly inside",
  );
  requireMalformedText(
    "hole-overlap",
    '{"id":"holes","new":{"type":"Polygon","coordinates":[[[0,0],[5,0],[5,5],[0,5],[0,0]],[[1,1],[3,1],[3,3],[1,3],[1,1]],[[2,2],[4,2],[4,4],[2,4],[2,2]]]}}\n',
    1,
    "overlap or nest",
  );
  console.log("verification complete");
} finally {
  rmSync(work, { recursive: true, force: true });
}
