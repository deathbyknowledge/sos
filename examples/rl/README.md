Make sure you have `sos serve` running!

Run training with:
```sh
uv run train --model <model-name>
```

Run benchmark with:
```sh
uv run benchmark
```

Define your model configs in `all_experiments.py`.
ENV vars:
```
EPHEMERAL=1 # Whether to delete and remove the sandbox after it's used. Set to 0 if you want to inspect them afterwards.
MAX_TURNS=30  # Max turns for a rollout
MAX_MODEL_TOKENS=32000
SHELLM=1 # Whether the model follows standard chat format or SHELLM.
```