# Evaluation

Run the same agent wrapper five times in raw, deterministic, and hybrid modes:

```sh
python3 evaluation/run.py --runs 5 --agent-command ./your-agent-wrapper.sh '{mode}' '{fixture}' '{data}'
```

The wrapper receives the mode, isolated fixture path, and isolated ledger path. It should configure Claude Code appropriately and must modify only the supplied fixture copy. Results are written as JSON and summarized by completion rate and runtime. This harness does not silently launch a `--yolo` agent; that decision stays explicit in the wrapper.
