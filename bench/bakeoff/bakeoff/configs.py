"""Configurations (which components are on) and model profiles (which model, through what).

A configuration is a set of components, each mapped to an existing, supported `stack-agent work`
setting or to a harness-side context mode. Nothing here changes what Chip validates or executes.

| component         | values                         | how                                              |
| ----------------- | ------------------------------ | ------------------------------------------------ |
| context           | brief / targeted / diagnosis   | the goal as written; plus the task's pre-written pointer; plus the failing tests the harness observed at the start |
| recall            | false / true                   | `--no-recall` / default episodic recall           |
| workflows         | false / true                   | `--workflows`                                     |
| state             | durable / none                 | `--state-db FILE --task-id ID` / `--no-state`     |
| prompt            | full / compact                 | `--decision-prompt`                               |
| structured_output | profile / object / none        | `--structured-output` (profile: the provider profile's own) |
"""

from __future__ import annotations

import os
from pathlib import Path

from . import CONFIG_SCHEMA
from .util import BENCH, read_json

CONFIG_DIR = BENCH / "bakeoff" / "configs"

COMPONENTS = {
    "context": ("brief", "targeted", "diagnosis"),
    "recall": (False, True),
    "workflows": (False, True),
    "state": ("durable", "none"),
    "prompt": ("full", "compact"),
    "structured_output": ("profile", "object", "none"),
}
DEFAULTS = {"context": "brief", "recall": False, "workflows": False, "state": "durable",
            "prompt": "full", "structured_output": "profile"}


def load_configs() -> dict[str, dict]:
    doc = read_json(CONFIG_DIR / "configurations.json")
    if doc.get("schema") != CONFIG_SCHEMA:
        raise ValueError("configurations.json: wrong schema")
    out = {}
    for c in doc["configurations"]:
        comps = {**DEFAULTS, **c.get("components", {})}
        for k, v in comps.items():
            if k not in COMPONENTS or v not in COMPONENTS[k]:
                raise ValueError(f"configuration {c['id']}: {k}={v!r} is not supported")
        if comps["state"] == "none" and (comps["recall"] or comps["workflows"]):
            raise ValueError(f"configuration {c['id']}: recall and workflows need durable state")
        out[c["id"]] = {**c, "components": comps}
    return out


def load_models() -> dict[str, dict]:
    doc = read_json(CONFIG_DIR / "models.json")
    return {m["id"]: m for m in doc["models"]}


def work_flags(components: dict) -> list[str]:
    flags = []
    if not components["recall"] and components["state"] == "durable":
        flags.append("--no-recall")
    if components["workflows"]:
        flags.append("--workflows")
    if components["prompt"] == "compact":
        flags += ["--decision-prompt", "compact"]
    if components["structured_output"] != "profile":
        flags += ["--structured-output", components["structured_output"]]
    return flags


def model_flags(model: dict) -> list[str]:
    flags = ["--provider", "openai-compatible", "--model", model["model"],
             "--provider-profile", model.get("profile", "openai"),
             "--max-output-tokens", str(model.get("max_output_tokens", 8192)),
             "--call-timeout-secs", str(model.get("call_timeout_secs", 120))]
    if model.get("structured_output"):
        flags += ["--structured-output", model["structured_output"]]
    return flags


def supported(model: dict, components: dict) -> bool:
    """Whether the model's provider supports every component value of a configuration."""
    for k, values in (model.get("unsupported") or {}).items():
        if components.get(k) in values:
            return False
    return True


def upstream_key(model: dict) -> str | None:
    var = model.get("key_env")
    return os.environ.get(var) if var else None


def model_available(model: dict) -> tuple[bool, str]:
    if model["kind"] == "scripted":
        return True, "scripted"
    if not upstream_key(model):
        return False, f"{model['key_env']} is not set"
    return True, "configured"


def config_path() -> Path:
    return CONFIG_DIR / "configurations.json"
