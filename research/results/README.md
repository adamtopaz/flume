# Results

Every standard harness run committed by an iteration, one JSON file each
(`<timestamp>-<machine>-<commit>.json`, written by `bench/harness/run.py`), never edited
afterwards. `LEADERBOARD.md` is generated from them by `bench/harness/leaderboard.py`
after every merge (a workflow commits it); do not edit it or commit it in a PR.
Absolute times are only comparable on one machine; the ratios between tools measured in
the same run are what carry across machines.
