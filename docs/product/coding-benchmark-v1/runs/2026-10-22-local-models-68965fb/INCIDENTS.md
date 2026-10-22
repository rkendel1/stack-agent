# Harness incidents (infrastructure, not model runs)

1. 2026-10-22 ~15:11 local: `run-one.sh 35b T2 1` aborted before invoking the agent: bash 3.2 + `set -u` reported
   `extra[@]: unbound variable` for the empty extra-args array (T2/T4 pass no extra flags; E passed its flags, so E r1 was unaffected).
   The agent, Locals and the model were never contacted for T2 r1 (no progress log, no exit record, no state). The orchestrator stopped.
   The never-started T2 r1 directory was removed and the script fixed (`${extra[@]+"${extra[@]}"}`); no run executed twice, no run result discarded.
   The run configuration is otherwise unchanged. The 35b E r1 run completed before the incident and is kept.
