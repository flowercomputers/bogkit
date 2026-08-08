#!/usr/bin/env node
import { writeFileSync } from "node:fs";

const output = process.argv[2];
if (!output) {
  process.stderr.write("usage: node scripts/generate-workload.ts OUTPUT.ndjson\n");
  process.exit(2);
}

function seeded(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 2 ** 32;
  };
}

const random = seeded(0x200faced);
const lines: string[] = [];

type Position = [number, number];
type Geometry = { type: "Polygon" | "MultiPolygon"; coordinates: unknown };

function ring(
  centerX: number,
  centerY: number,
  vertices: number,
  radius: number,
  alternating = true,
): Position[] {
  const positions: Position[] = [];
  for (let vertex = 0; vertex < vertices; vertex += 1) {
    const angle = (vertex / vertices) * Math.PI * 2;
    const localRadius = alternating && vertex % 2 === 1 ? radius * 0.72 : radius;
    positions.push([
      centerX + Math.cos(angle) * localRadius,
      centerY + Math.sin(angle) * localRadius,
    ]);
  }
  positions.push(positions[0]);
  return positions;
}

function geometry(
  mode: number,
  centerX: number,
  centerY: number,
  vertices: number,
  radius: number,
): Geometry {
  if (mode === 1) {
    const holeVertices = Math.max(4, Math.floor(vertices / 4));
    const outerVertices = vertices - holeVertices;
    return {
      type: "Polygon",
      coordinates: [
        ring(centerX, centerY, outerVertices, radius),
        ring(centerX, centerY, holeVertices, radius * 0.24, false),
      ],
    };
  }
  if (mode === 2) {
    const firstVertices = Math.floor(vertices / 2);
    const secondVertices = vertices - firstVertices;
    return {
      type: "MultiPolygon",
      coordinates: [
        [ring(centerX - radius * 1.6, centerY, firstVertices, radius)],
        [ring(centerX + radius * 1.6, centerY, secondVertices, radius)],
      ],
    };
  }
  return { type: "Polygon", coordinates: [ring(centerX, centerY, vertices, radius)] };
}

for (let edit = 0; edit < 1_000; edit += 1) {
  const centerX = -74.10 + random() * 0.20;
  const centerY = 40.65 + random() * 0.20;
  const radii = [0.0002, 0.001, 0.004, 0.01];
  const radius = radii[edit % radii.length];
  const mode = Math.floor(edit / 3) % 3;
  const operation = edit % 3;
  const entry: Record<string, unknown> = { id: `load-${edit}` };
  if (operation === 0) {
    entry.new = geometry(mode, centerX, centerY, 200, radius);
  } else if (operation === 1) {
    entry.old = geometry(mode, centerX, centerY, 200, radius);
  } else {
    entry.old = geometry(mode, centerX, centerY, 100, radius);
    entry.new = geometry(mode, centerX + radius * 0.3, centerY - radius * 0.2, 100, radius);
  }
  lines.push(JSON.stringify(entry));
}
writeFileSync(output, `${lines.join("\n")}\n`);
process.stdout.write(
  `wrote 1000 mixed-operation edits with 200 distinct vertices per line to ${output}\n`,
);
