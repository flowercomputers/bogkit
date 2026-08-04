#!/usr/bin/env python3
"""Independent, deliberately small semantic oracle for the fixture catalog.

This does not invoke, import, or reproduce the Rust parser. It reasons over
already-valid fixture schemas and checks language inclusion by structural rules.
"""

import json
import sys
from pathlib import Path


def first_break(producer, consumer):
    if producer["type"] != consumer["type"]:
        return "type-mismatch"

    kind = producer["type"]
    if kind == "string":
        consumer_enum = consumer.get("enum")
        if consumer_enum is None:
            return None
        producer_enum = producer.get("enum")
        if producer_enum is None or not set(producer_enum).issubset(consumer_enum):
            return "enum-value-rejected"
        return None

    if kind == "integer":
        producer_min = producer.get("minimum")
        producer_max = producer.get("maximum")
        consumer_min = consumer.get("minimum")
        consumer_max = consumer.get("maximum")
        if consumer_min is not None and (producer_min is None or producer_min < consumer_min):
            return "integer-below-minimum"
        if consumer_max is not None and (producer_max is None or producer_max > consumer_max):
            return "integer-above-maximum"
        return None

    if kind == "array":
        return first_break(producer["items"], consumer["items"])

    if kind == "object":
        producer_properties = producer.get("properties", {})
        consumer_properties = consumer.get("properties", {})
        producer_required = set(producer.get("required", []))
        consumer_required = set(consumer.get("required", []))

        for name in sorted(consumer_required):
            consumer_has_default = "default" in consumer_properties[name]
            producer_always_has = (
                name in producer_required
                and name in producer_properties
                and "default" not in producer_properties[name]
            )
            if not consumer_has_default and not producer_always_has:
                return "required-field-missing"

        for name in sorted(producer_properties):
            if name in consumer_properties:
                nested = first_break(producer_properties[name], consumer_properties[name])
                if nested:
                    return nested
            elif not consumer.get("additionalProperties", True):
                return "closed-object-rejects-property"

        if producer.get("additionalProperties", True):
            if not consumer.get("additionalProperties", True):
                return "closed-object-rejects-property"
            if any(name not in producer_properties for name in consumer_properties):
                return "open-object-property-unconstrained"
        return None

    raise AssertionError(f"fixture uses unsupported type {kind!r}")


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: oracle.py <semantic_cases.json>")
    fixture = json.loads(Path(sys.argv[1]).read_text())
    schemas = fixture["schemas"]
    cases = fixture["cases"]
    if len(cases) < 60:
        raise AssertionError(f"fixture regression: only {len(cases)} cases")

    failures = []
    for case in cases:
        rule = first_break(schemas[case["producer"]], schemas[case["consumer"]])
        actual = "allow" if rule is None else "block"
        if actual != case["expected"] or (rule is not None and rule != case["rule"]):
            failures.append(
                f'{case["name"]}: expected {case["expected"]}/{case.get("rule")}, '
                f"oracle got {actual}/{rule}"
            )
    if failures:
        raise AssertionError("\n".join(failures))
    print(f"independent oracle verified {len(cases)} semantic cases")


if __name__ == "__main__":
    main()
