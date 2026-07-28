# BogKit developer simulation lab

This directory is a living research corpus for daily trials by simulated
developers who encounter BogKit without prior product knowledge.

The goal is product feedback, not example volume. Every trial begins with an
existing software problem and asks whether BogKit makes the solution measurably
better, clearer, or simpler. A trial may conclude that BogKit is not a good fit.

## Boundaries

- Daily work stays under `developer-simulation/`.
- The automation never changes `fold`, `anny`, `ese`, existing examples, or
  other BogKit files.
- Core changes remain proposals until a maintainer explicitly approves them.
- Prototypes contain no credentials, private data, generated build output,
  large binary fixtures, or unjustified dependencies.
- A failed integration is reduced to the smallest useful reproducer.

## Daily protocol

1. Sync the simulation branch with the latest `origin/main` without rewriting
   history.
2. Read `coverage.json` and prior reports. Select underexplored combinations of
   developer role, domain, workload, constraints, and BogKit components.
3. Ask a scenario designer that has not inspected BogKit to produce two
   realistic briefs. Each brief must define:
   - the developer's role and level of Rust experience;
   - an existing system and its current approach;
   - the concrete problem, constraints, and baseline;
   - acceptance criteria and explicit non-goals.
4. Give each brief to a fresh simulator in a sanitized checkout of current
   `main`. The simulator starts at the public README and examples. It must not
   read prior simulation runs before finishing its own attempt.
5. Each simulator builds the smallest meaningful prototype, runs it, tests it,
   and records its discovery and debugging trail. It may adopt part of BogKit
   or reject the toolkit.
6. A separate skeptical reviewer reproduces important claims, challenges
   unnecessary choices, and rejects vague or unsupported recommendations.
7. Archive accepted prototypes in `runs/`, write the daily synthesis in
   `reports/`, update `coverage.json`, and validate the full corpus.
8. Commit and push the evidence before adding the immutable daily PR comment
   and updating the rolling dashboard comment.

## Archive contract

Runnable trials live at:

```text
runs/YYYY-MM-DD--short-slug/
```

Each runnable directory is a member of this nested Cargo workspace and includes
its own README. Package names must be unique. Path dependencies point to the
current BogKit checkout so the archived code exercises the branch being tested.

Daily reports live at:

```text
reports/YYYY-MM-DD.md
```

The report follows `REPORT_TEMPLATE.md`. Exact commands and observed results are
required. Performance claims require a stated baseline, release-mode runs, and
enough repetitions to avoid presenting a one-off timing as a conclusion.

## Finding policy

Findings use one of these categories:

- correctness defect
- performance problem
- API friction
- documentation gap
- missing capability
- poor product fit

Every finding includes evidence, severity, confidence, a reproduction path, and
the smallest plausible improvement. Prefer documentation or examples before a
new API, and a small API correction before a new subsystem.

A feature suggestion becomes a dashboard candidate only when:

- two independent trials encounter the same need; or
- one trial demonstrates a serious reproducible correctness problem.

One-off ideas remain observations.

## Idempotency

Each daily report and PR comment uses the marker:

```text
<!-- bogkit-developer-simulation:YYYY-MM-DD -->
```

If the dated report and comment already exist, a repeated run does nothing. If
the branch evidence exists but the comment is missing, the coordinator posts
the missing comment without generating new trials.
