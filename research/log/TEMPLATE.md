# <slug>: <one line: what this iteration set out to do>

- **Date:** YYYY-MM-DD
- **Author:** <name or agent, and the machine id from the result file>
- **Milestone:** <roadmap id and step, e.g. C1 (path dependencies)>
- **Outcome:** success | milestone | failure | inconclusive
- **Branch / PR:** research/<slug>: <PR URL>
- **Result files:** research/results/<file>.json (every standard run you made)

## Plan

What you expected to change, which scenarios or projects it affects, and why
(profiling evidence, earlier ideas it builds on).

## What was done

The change in a few sentences. Files touched. Tests added. Anything surprising.

## Measurements

The harness's `speedup of candidate vs main` and `vs lake` lines, the `correctness`
line, and the `verdict` lines, verbatim. Caps (cpus, mem), suite, targets, repetitions.
If you reran for noise, say so and list every result file.

## Conclusion

Why it worked or did not, and what the next person should try or avoid.
