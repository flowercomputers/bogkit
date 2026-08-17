# peat hooks — Claude Code integration

Verified against the Claude Code hooks docs (2026-08-16). The contract differs
from early drafts of the spec in one important way: **there are no
`$CLAUDE_TRANSCRIPT_PATH` / `$CLAUDE_SESSION_ID` environment variables.** Hook
commands receive a JSON object on **stdin**; extract fields with `jq`.

## Stdin contract (fields we use)

Both `SessionStart` and `Stop` receive at least:

```json
{
  "session_id": "abc123",
  "hook_event_name": "SessionStart | Stop",
  "cwd": "/path/to/project",
  "transcript_path": "/Users/you/.claude/projects/<slug>/<session>.jsonl"
}
```

`Stop` additionally carries `last_assistant_message` (final text of the turn —
useful because the transcript file may lag) and `stop_hook_active` (guard
against re-entry). `SessionStart` supports a `matcher` on how the session
started: `startup | resume | clear | compact | fork`.

## Stdout treatment

`SessionStart` is one of the few hooks whose **plain-text stdout is added to
the session context** — exactly what `peat brief` wants. `Stop` stdout goes to
the debug log only, so `peat capture` output is invisible; rely on exit code 0.

## Snippet — `.claude/settings.json` (project)

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "in=$(cat); printf '%s' \"$in\" | jq -r '.session_id' > .peat/current-session 2>/dev/null; peat brief 2>/dev/null || true"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "in=$(cat); tp=$(printf '%s' \"$in\" | jq -r '.transcript_path // empty'); fm=$(printf '%s' \"$in\" | jq -r '.last_assistant_message // empty'); [ -n \"$tp\" ] && peat capture \"$tp\" --final-msg \"$fm\" 2>/dev/null || true"
          }
        ]
      }
    ]
  }
}
```

Notes:

- **peat failing may never break a session** — every command ends `|| true`.
- The Stop hook reads stdin **once** and extracts both `transcript_path` and
  `last_assistant_message`; the latter is passed as `--final-msg`, which is
  authoritative over transcript tail parsing (the transcript file may lag the
  final turn). The empty-path guard skips capture rather than erroring.
- The SessionStart hook writes `.peat/current-session` so that `peat obs`
  (run by the agent mid-session, which has no session id in its environment)
  can resolve the session without a `--session` flag. `$CLAUDE_SESSION_ID`
  does not exist; this file is the substitute. `.peat/` is gitignored.
- Task words for `brief`: SessionStart stdin has no user prompt (the session
  hasn't started). v1 correctly skips the `relevant` section on hook-invoked
  briefs; `peat brief <words>` remains available for manual use.
- Consider `"matcher": "startup|clear"` on SessionStart if resume/compact
  re-briefing gets noisy.

## Obs guidance (append to the project's CLAUDE.md/AGENTS.md)

```markdown
### peat observations

At commit points and task completions, deposit a one-line observation:

    peat obs <subject> "<what you now believe>" [--from seq,seq]

Cite transcript seqs with --from when the belief derives from specific events;
an uncited obs is visibly a bare assertion.
```
