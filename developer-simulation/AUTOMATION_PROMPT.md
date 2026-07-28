# Daily automation instructions

Run the BogKit developer simulation lab for the current date in
`America/New_York`.

The user explicitly authorized multi-agent work for this automation. Use a
scenario designer, two independent simulated developers, and a skeptical
reviewer. Keep the coordinator responsible for repository and GitHub writes.

## Fixed targets

- Repository: `flowercomputers/bogkit`
- Base branch: `main`
- Simulation branch: `ed/developer-simulation`
- Pull request: the open PR whose head is `ed/developer-simulation`
- Archive root: `developer-simulation/`
- Daily PR marker: `<!-- bogkit-developer-simulation:YYYY-MM-DD -->`
- Dashboard marker: `<!-- bogkit-developer-simulation-dashboard -->`

## Safety boundary

- Modify only `developer-simulation/`.
- Never automatically edit BogKit core, existing examples, root workspace
  configuration, or repository-wide documentation.
- Never force-push, merge the PR, create another PR, or open issues.
- Never commit credentials, private data, generated build output, databases,
  large binary fixtures, or unjustified dependencies.
- Stop and report a blocker if current changes, merge conflicts, missing GitHub
  access, or an unexpected branch state make a safe run uncertain.

## Preflight and idempotency

1. Read `developer-simulation/README.md`, `coverage.json`, and recent reports.
2. Verify GitHub access and resolve the open draft PR by its exact head branch.
3. Fetch `origin`. Start from the current remote simulation branch and merge the
   latest `origin/main` without rewriting history. Abort safely on conflict.
4. Search both the branch and PR conversation for today's daily marker.
   - If both exist, make no changes and finish successfully.
   - If the report exists but the PR comment is missing, publish the existing
     report, refresh the dashboard, and do not generate new trials.
   - If a partial dated archive exists without a finished report, inspect and
     resume only confirmed work. Do not create a second dated archive.

## Scenario design

Spawn a scenario-designer subagent without inherited conversation context. Do
not let it inspect BogKit. Give it only the coverage ledger and ask for two
substantially different, underexplored existing-software problems.

Each scenario must specify:

- a developer role and Rust experience level;
- the existing system and baseline implementation;
- the concrete pain, workload, data shape, and operational constraints;
- measurable acceptance criteria and explicit non-goals;
- a compact, self-contained prototype boundary.

Do not reverse-engineer scenarios around Fold, ESE, or ANNy. Avoid repeating
games, generic agent memory, media search, or repository search unless the
coverage ledger shows a materially new workload or constraint.

## Independent developer trials

Create two separate sanitized temporary checkouts of current `origin/main`.
They must not contain `developer-simulation/` or prior lab reports.

Spawn one fresh simulator subagent per checkout without inherited conversation
context. Give each only its persona, problem brief, assigned checkout, and these
rules:

- begin with the public README and examples;
- behave as a developer with no prior BogKit knowledge;
- do not read the other trial or prior lab work;
- evaluate the stated baseline before choosing BogKit;
- use only the BogKit components that fit, and allow a no-fit conclusion;
- build the smallest meaningful runnable prototype or failure reproducer;
- test, format, lint with warnings denied, and run the demonstration;
- record the ordered discovery and friction trail, exact commands and observed
  results, categorized findings, and a decision audit;
- make no repository, GitHub, or automation writes.

Run the simulators in parallel only when their directories are disjoint.

## Skeptical review

Spawn a separate reviewer after both trials finish. The reviewer may inspect
both prototypes, current BogKit source, and prior reports, but makes no
repository or GitHub writes.

Require the reviewer to:

- reproduce important behavior and any serious defect;
- compare each solution with the scenario's baseline;
- reject, soften, or relabel claims that exceed the evidence;
- identify unnecessary dependencies, abstractions, or test scope;
- audit consequential choices and unresolved uncertainty;
- enforce the finding and candidate thresholds in `README.md`.

The coordinator fixes rejected quality problems and reruns validation before
archiving.

## Archive and validation

Archive accepted runnable prototypes in flat, uniquely named directories:

```text
developer-simulation/runs/YYYY-MM-DD--short-slug/
```

Each runnable Rust prototype must be a member of the nested workspace, use path
dependencies on the checkout being tested, and include a README with exact
reproduction instructions. Archive a blocked trial only as its minimal
reproducer.

Write one synthesis at `developer-simulation/reports/YYYY-MM-DD.md` following
`REPORT_TEMPLATE.md`. Update `coverage.json` with both trials and independently
recurring findings.

Before publication:

1. Run each new prototype's tests, formatting check, strict lint, and demo.
2. Run the nested lab workspace tests and strict lint.
3. Run `cargo test --workspace` from the BogKit root.
4. Run `git diff --check`.
5. Inspect every changed path and confirm it is under `developer-simulation/`.
6. Scan the archive for secrets, generated databases, build output, and large
   or binary files.

## Publication

Commit the dated archive with:

```text
simulation: YYYY-MM-DD developer trials
```

Push normally to `origin/ed/developer-simulation`. If the push is rejected,
fetch and inspect the divergence; do not force-push.

After the commit is confirmed on the remote:

1. Append one top-level PR comment containing the daily report and its marker.
2. Find the existing dashboard comment by its marker and replace its body with
   a compact current dashboard:
   - total trials and outcome counts;
   - coverage summary;
   - confirmed defects;
   - recurring API or documentation friction;
   - candidate improvements that meet the threshold;
   - no-fit and positioning signals;
   - links to every dated report comment and branch report.
3. Verify the daily comment and dashboard through a fresh read.

If the branch push succeeds but commenting fails, keep the branch evidence,
retry only the missing comment step, and fail visibly. Never generate duplicate
trials to repair a reporting failure.
