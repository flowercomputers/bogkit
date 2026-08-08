#!/usr/bin/env node

// Dependency-free TypeScript-style reference evaluator. It is intentionally
// separate from the Rust library so the golden suite exercises cross-language
// rule and percentage semantics rather than only replaying Rust output.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

const fixtureDirectory = process.argv[2];
if (!fixtureDirectory) {
  throw new Error("usage: node baseline/reference-evaluator.mjs FIXTURE_DIR");
}

const snapshot = readJson("snapshot.json");
const reordered = readJson("snapshot-reordered.json");
const golden = readJson("golden.json");

for (const testCase of golden) {
  const actual = evaluate(snapshot, testCase.input.flag, testCase.input.context);
  const reorderedActual = evaluate(
    reordered,
    testCase.input.flag,
    testCase.input.context,
  );
  assert.deepStrictEqual(actual, testCase.expected, testCase.input.case_id);
  assert.deepStrictEqual(
    reorderedActual,
    actual,
    `${testCase.input.case_id} reordered`,
  );
}

assert.equal(stableBucket("salt", "flag", "rule", "user-42"), 7307);
console.log(
  `TypeScript-style reference matched ${golden.length} golden cases and reordered-object decisions; known bucket=7307`,
);

function readJson(name) {
  return JSON.parse(fs.readFileSync(path.join(fixtureDirectory, name), "utf8"));
}

function evaluate(config, flagKey, context) {
  const flag = config.flags[flagKey];
  if (!flag) throw new Error(`unknown flag ${JSON.stringify(flagKey)}`);
  const explanation = [];

  for (const rule of flag.rules) {
    let failure;
    for (const condition of rule.conditions ?? []) {
      if (!Object.hasOwn(context, condition.attribute)) {
        failure = `missing attribute ${JSON.stringify(condition.attribute)}`;
        break;
      }
      const actual = context[condition.attribute];
      if (!conditionMatches(actual, condition)) {
        failure = `attribute ${JSON.stringify(condition.attribute)} was ${display(actual)}; condition did not match`;
        break;
      }
    }

    if (failure) {
      explanation.push({ rule_id: rule.id, matched: false, reason: failure });
      continue;
    }

    if (rule.percentage) {
      const attribute = rule.percentage.attribute;
      if (!Object.hasOwn(context, attribute)) {
        explanation.push({
          rule_id: rule.id,
          matched: false,
          reason: `missing percentage attribute ${JSON.stringify(attribute)}`,
        });
        continue;
      }
      const bucket = stableBucket(
        config.salt,
        flagKey,
        rule.id,
        bucketKey(context[attribute]),
      );
      if (bucket >= rule.percentage.basis_points) {
        explanation.push({
          rule_id: rule.id,
          matched: false,
          reason: `stable bucket ${bucket} was outside 0..${rule.percentage.basis_points}`,
        });
        continue;
      }
      explanation.push({
        rule_id: rule.id,
        matched: true,
        reason: `conditions matched; stable bucket ${bucket} was inside 0..${rule.percentage.basis_points}`,
      });
    } else {
      explanation.push({
        rule_id: rule.id,
        matched: true,
        reason: "all conditions matched",
      });
    }

    return {
      flag: flagKey,
      value: rule.serve,
      source: rule.id,
      explanation,
    };
  }

  explanation.push({
    rule_id: "default",
    matched: true,
    reason: "no targeting rule matched",
  });
  return {
    flag: flagKey,
    value: flag.default,
    source: "default",
    explanation,
  };
}

function conditionMatches(actual, condition) {
  switch (condition.op) {
    case "eq":
      return actual === condition.value;
    case "not_eq":
      return actual !== condition.value;
    case "greater_than":
      return typeof actual === "number" && actual > condition.value;
    default:
      throw new Error(`unknown operator ${condition.op}`);
  }
}

function display(value) {
  return typeof value === "string" ? JSON.stringify(value) : String(value);
}

function bucketKey(value) {
  if (typeof value === "boolean") return `b:${value}`;
  if (typeof value === "number") return `n:${value}`;
  return `s:${value}`;
}

function stableBucket(salt, flag, rule, attribute) {
  let hash = 0xcbf29ce484222325n;
  for (const part of [salt, flag, rule, attribute]) {
    for (const byte of Buffer.from(part)) {
      hash ^= BigInt(byte);
      hash = BigInt.asUintN(64, hash * 0x100000001b3n);
    }
    hash = BigInt.asUintN(64, hash * 0x100000001b3n);
  }
  return Number(hash % 10000n);
}
