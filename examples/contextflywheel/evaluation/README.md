# Evaluation

Compare raw transcript, deterministic ContextFlywheel, and hybrid ContextFlywheel on an isolated fixture copy. The wrapper must edit only that copy.

```sh
python3 evaluation/run.py --runs 5 \
  --agent-command ./your-agent-wrapper.sh '{mode}' '{fixture}' '{data}'
```

Use the harder isolated fixture when you intend to launch a real local agent:

```sh
python3 evaluation/run.py --fixture evaluation/hidden_fixture --runs 3 \
  --agent-command evaluation/local-qwen-wrapper.sh '{mode}' '{fixture}' '{data}'
```

`run.py` records success, runtime, ledger belief/tool counts, optional provider usage from Claude JSON stdout, and writes `evaluation-results.json`. Treat a one-run pilot as a pilot, not a performance claim.

This harness does not silently launch `--yolo` or `--dangerously-skip-permissions`; that stays in the wrapper you pass.
