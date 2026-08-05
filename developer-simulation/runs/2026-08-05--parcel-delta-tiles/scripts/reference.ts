#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

type Point = [number, number];
type Ring = Point[];
type Polygon = Ring[];
type MultiPolygon = Polygon[];
type Rect = { minX: number; minY: number; maxX: number; maxY: number };

const MIN_ZOOM = 12;
const MAX_ZOOM = 16;
const MAX_MERCATOR_LAT = 85.0511287798066;
const EPS = 1e-12;

function fail(message: string): never {
  throw new Error(message);
}

function parsePosition(value: unknown, path: string): Point {
  if (!Array.isArray(value) || value.length !== 2) {
    fail(`${path} must contain exactly longitude and latitude`);
  }
  const [longitude, latitude] = value as unknown[];
  if (typeof longitude !== "number" || !Number.isFinite(longitude)) {
    fail(`${path}[0] must be a finite number`);
  }
  if (typeof latitude !== "number" || !Number.isFinite(latitude)) {
    fail(`${path}[1] must be a finite number`);
  }
  if (longitude < -180 || longitude > 180) {
    fail(`${path}[0] longitude is outside [-180, 180]`);
  }
  if (latitude < -MAX_MERCATOR_LAT || latitude > MAX_MERCATOR_LAT) {
    fail(`${path}[1] latitude is outside Web Mercator limits`);
  }
  return [longitude, latitude];
}

function parseRing(value: unknown, path: string): Ring {
  if (!Array.isArray(value) || value.length < 4) {
    fail(`${path} must contain at least four positions`);
  }
  const ring = value.map((position, index) => parsePosition(position, `${path}[${index}]`));
  const first = ring[0];
  const last = ring[ring.length - 1];
  if (first[0] !== last[0] || first[1] !== last[1]) {
    fail(`${path} is open; first and last positions must match`);
  }
  return ring;
}

function parsePolygon(value: unknown, path: string): Polygon {
  if (!Array.isArray(value) || value.length === 0) {
    fail(`${path} must contain an exterior ring`);
  }
  return value.map((ring, index) => parseRing(ring, `${path}[${index}]`));
}

function parseGeometry(value: unknown, field: string): MultiPolygon {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    fail(`${field} geometry must be an object`);
  }
  const object = value as Record<string, unknown>;
  let polygons: MultiPolygon;
  if (object.type === "Polygon") {
    polygons = [parsePolygon(object.coordinates, `${field}.coordinates`)];
  } else if (object.type === "MultiPolygon") {
    if (!Array.isArray(object.coordinates) || object.coordinates.length === 0) {
      fail(`${field}.coordinates must contain at least one polygon`);
    }
    polygons = object.coordinates.map((polygon, index) =>
      parsePolygon(polygon, `${field}.coordinates[${index}]`),
    );
  } else {
    fail(`${field}.type ${JSON.stringify(object.type)} is unsupported`);
  }
  const longitudes = polygons.flat(2).map((point) => point[0]);
  if (Math.max(...longitudes) - Math.min(...longitudes) > 180) {
    fail(`${field} crosses the antimeridian, which this prototype does not support`);
  }
  return polygons;
}

export function parseNdjson(text: string): MultiPolygon[] {
  const lines = text.split(/\r?\n/);
  if (lines.at(-1) === "") lines.pop();
  const geometries: MultiPolygon[] = [];
  for (const [index, line] of lines.entries()) {
    const lineNumber = index + 1;
    if (line.trim() === "") fail(`input line ${lineNumber}: blank line`);
    try {
      const value: unknown = JSON.parse(line);
      if (value === null || typeof value !== "object" || Array.isArray(value)) {
        fail("edit must be a JSON object");
      }
      const edit = value as Record<string, unknown>;
      if (typeof edit.id !== "string" || edit.id.length === 0) {
        fail("id must be a non-empty string");
      }
      let count = 0;
      for (const field of ["old", "new"] as const) {
        if (edit[field] !== undefined && edit[field] !== null) {
          geometries.push(parseGeometry(edit[field], field));
          count += 1;
        }
      }
      if (count === 0) fail("at least one of old or new must contain a geometry");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (message.startsWith("input line ")) throw error;
      fail(`input line ${lineNumber}: ${message}`);
    }
  }
  return geometries;
}

function bounds(polygon: Polygon): Rect {
  const points = polygon.flat();
  return {
    minX: Math.min(...points.map(([x]) => x)),
    minY: Math.min(...points.map(([, y]) => y)),
    maxX: Math.max(...points.map(([x]) => x)),
    maxY: Math.max(...points.map(([, y]) => y)),
  };
}

function lonToWorldX(longitude: number, n: number): number {
  return ((longitude + 180) / 360) * n;
}

function latToWorldY(latitude: number, n: number): number {
  const radians = (latitude * Math.PI) / 180;
  return ((1 - Math.asinh(Math.tan(radians)) / Math.PI) / 2) * n;
}

function touchingRange(min: number, max: number, n: number): [number, number] {
  const nearInteger = (value: number) => Math.abs(value - Math.round(value)) <= 1e-10;
  const lower = nearInteger(min) ? Math.round(min) - 1 : Math.floor(min);
  const upper = nearInteger(max) ? Math.round(max) : Math.floor(max);
  return [Math.max(0, lower), Math.min(n - 1, upper)];
}

function worldYToLatitude(y: number, n: number): number {
  return (Math.atan(Math.sinh(Math.PI * (1 - (2 * y) / n))) * 180) / Math.PI;
}

function tileRect(z: number, x: number, y: number): Rect {
  const n = 2 ** z;
  return {
    minX: (x / n) * 360 - 180,
    maxX: ((x + 1) / n) * 360 - 180,
    minY: worldYToLatitude(y + 1, n),
    maxY: worldYToLatitude(y, n),
  };
}

function segmentIntersectsRect([ax, ay]: Point, [bx, by]: Point, rect: Rect): boolean {
  const dx = bx - ax;
  const dy = by - ay;
  let tMin = 0;
  let tMax = 1;
  const clips: [number, number][] = [
    [-dx, ax - rect.minX],
    [dx, rect.maxX - ax],
    [-dy, ay - rect.minY],
    [dy, rect.maxY - ay],
  ];
  for (const [p, q] of clips) {
    if (Math.abs(p) <= EPS) {
      if (q < -EPS) return false;
    } else {
      const ratio = q / p;
      if (p < 0) tMin = Math.max(tMin, ratio);
      else tMax = Math.min(tMax, ratio);
      if (tMin - tMax > EPS) return false;
    }
  }
  return true;
}

type Location = "outside" | "inside" | "boundary";

function pointOnSegment([px, py]: Point, [ax, ay]: Point, [bx, by]: Point): boolean {
  const cross = (bx - ax) * (py - ay) - (by - ay) * (px - ax);
  const scale = Math.abs(bx - ax) + Math.abs(by - ay) + 1;
  return (
    Math.abs(cross) <= EPS * scale &&
    px >= Math.min(ax, bx) - EPS &&
    px <= Math.max(ax, bx) + EPS &&
    py >= Math.min(ay, by) - EPS &&
    py <= Math.max(ay, by) + EPS
  );
}

function pointInRing(point: Point, ring: Ring): Location {
  let inside = false;
  for (let index = 0; index + 1 < ring.length; index += 1) {
    const a = ring[index];
    const b = ring[index + 1];
    if (pointOnSegment(point, a, b)) return "boundary";
    if ((a[1] > point[1]) !== (b[1] > point[1])) {
      const crossingX = ((b[0] - a[0]) * (point[1] - a[1])) / (b[1] - a[1]) + a[0];
      if (crossingX > point[0]) inside = !inside;
    }
  }
  return inside ? "inside" : "outside";
}

function pointInFilledPolygon(point: Point, polygon: Polygon): boolean {
  const exterior = pointInRing(point, polygon[0]);
  if (exterior === "outside") return false;
  if (exterior === "boundary") return true;
  return !polygon.slice(1).some((hole) => pointInRing(point, hole) === "inside");
}

function polygonIntersectsRect(polygon: Polygon, rect: Rect): boolean {
  for (const ring of polygon) {
    for (let index = 0; index + 1 < ring.length; index += 1) {
      if (segmentIntersectsRect(ring[index], ring[index + 1], rect)) return true;
    }
  }
  const corners: Point[] = [
    [rect.minX, rect.minY],
    [rect.minX, rect.maxY],
    [rect.maxX, rect.minY],
    [rect.maxX, rect.maxY],
  ];
  return corners.some((corner) => pointInFilledPolygon(corner, polygon));
}

export function fullScanPlan(geometries: MultiPolygon[]): string[] {
  const polygons = geometries.flat();
  const countyBounds = polygons.map(bounds).reduce((a, b) => ({
    minX: Math.min(a.minX, b.minX),
    minY: Math.min(a.minY, b.minY),
    maxX: Math.max(a.maxX, b.maxX),
    maxY: Math.max(a.maxY, b.maxY),
  }));
  const output: string[] = [];
  for (let z = MIN_ZOOM; z <= MAX_ZOOM; z += 1) {
    const n = 2 ** z;
    const [xStart, xEnd] = touchingRange(
      lonToWorldX(countyBounds.minX, n),
      lonToWorldX(countyBounds.maxX, n),
      n,
    );
    const yA = latToWorldY(countyBounds.minY, n);
    const yB = latToWorldY(countyBounds.maxY, n);
    const [yStart, yEnd] = touchingRange(Math.min(yA, yB), Math.max(yA, yB), n);
    for (let x = xStart; x <= xEnd; x += 1) {
      for (let y = yStart; y <= yEnd; y += 1) {
        const rect = tileRect(z, x, y);
        if (polygons.some((polygon) => polygonIntersectsRect(polygon, rect))) {
          output.push(`${z}/${x}/${y}`);
        }
      }
    }
  }
  return output.sort();
}

function main(): void {
  const path = process.argv[2] ?? "-";
  const text = path === "-" ? readFileSync(0, "utf8") : readFileSync(path, "utf8");
  const output = fullScanPlan(parseNdjson(text));
  if (output.length > 0) process.stdout.write(`${output.join("\n")}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    main();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    process.stderr.write(`${message}\n`);
    process.exitCode = 1;
  }
}
