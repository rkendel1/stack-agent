# pydantic-ai experiment (Tier 3 screen)

Harness: `harness.py` — Pydantic AI `FunctionModel` scripted fake model (reply chosen from the
restored history, per the protocol's shared fixture), fake `lookup`/`charge` tools writing an
fsynced `ledger.jsonl`, and Pydantic AI Harness `StepPersistence(store=FileStepStore(...))`.
Kill points are deterministic `os.kill(getpid(), SIGKILL)` calls inside the tool / model function.

Packages: `pydantic-ai-slim==2.57.0`, `pydantic-ai-harness==0.57.0` from PyPI in a venv outside
the repo. Source reviewed at pydantic/pydantic-ai@60c1d1a6.

Run (per window, each in a fresh work dir):

```
PYDANTIC_AI_NO_BANNER=1 python -I harness.py run    <workdir> <none|invalid|W1|W2|W3|W6>
PYDANTIC_AI_NO_BANNER=1 python -I harness.py resume <workdir> <W1|W2|W3|W6>
```

Result summary (`results.json`): E1 verified; invalid args → one validation retry, tool never
called with bad args. E2: W1 correct, **W2 duplicate_effect** under naive continuation even
though the tool-effect ledger reported `charge started` (unresolved) — StepPersistence detects
the ambiguous effect but does not block replay; W3 correct; W6 correct only because the
application stored the suspended history itself.
