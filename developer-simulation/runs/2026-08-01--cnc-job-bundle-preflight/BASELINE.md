# Concrete baseline

The baseline is a small Python preflight that opens a ZIP, requires an exact
`manifest.json` member name, and rejects archive members whose filename does
not end in `.json`, `.nc`, or `.gcode` (case-insensitive). It does not parse
the manifest, read member contents, calculate checksums, compare declared byte
counts, check tools, reject unsafe paths, detect case-colliding names, enforce
a bundle-size policy, or stage files.

Expected baseline classifications before implementation:

| Fixture | Baseline expectation | Required classification |
| --- | --- | --- |
| valid | ready | ready |
| truncated member | ready | invalid |
| checksum mismatch | ready | invalid |
| undeclared file with allowed extension | ready | invalid |
| missing declared file | ready | invalid |
| duplicate path differing only by case | ready | invalid |
| absolute path | ready | invalid |
| parent traversal | ready | invalid |
| oversized bundle | ready | invalid |
| undeclared disallowed extension | invalid | invalid |

This is deliberately concrete and runnable, but it represents only the stated
existing checks. It is not presented as a safe implementation.

Measured correction: Python 3.14.6's `zipfile.ZipFile` rejected the physically
truncated fixture while opening it, so the measured baseline result for that
row was invalid rather than the predicted ready. It marked valid and every
other adversarial fixture ready. The prediction is retained above to make the
discovery trail explicit rather than retroactively changing the baseline.
