//! The project profile: typed facts about the project and its environment, each with the
//! observations that support it.
//!
//! Profiling reads files at the project root, file names under it (bounded), and facts the caller
//! established without executing anything new (where `pax` and `git` are on `PATH`, whether a model
//! was selected). It runs no project code, no build tool and no network request. For the same
//! inputs and [`PROFILE_VERSION`] it produces the same profile, byte for byte.
//!
//! The profile is an input to selection, not a source of authority: nothing in it authorizes a
//! capability.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

pub const PROFILE_VERSION: &str = "profile.v1";

/// How many entries under the root are examined for test layout. Beyond it the fact is unknown.
const MAX_SCANNED: usize = 2_000;

/// One thing the profiler saw, and where.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Observation {
    /// `file:<relative path>`, `dir:<relative path>`, `path:<tool>` or `runtime:<fact>`.
    pub source: String,
    pub fact: String,
}

/// Whether an attribute was seen directly or derived from what was seen, and by which rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "basis", rename_all = "snake_case")]
pub enum Basis {
    Observed,
    Inferred { rule: &'static str },
}

/// A profile attribute. Unknown and ambiguous are values, never guessed away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Attr<T> {
    Known {
        value: T,
        #[serde(flatten)]
        basis: Basis,
        evidence: Vec<String>,
    },
    Ambiguous {
        candidates: Vec<T>,
        evidence: Vec<String>,
    },
    Unknown {
        reason: String,
    },
}

impl<T: PartialEq> Attr<T> {
    pub fn is(&self, wanted: &T) -> bool {
        matches!(self, Attr::Known { value, .. } if value == wanted)
    }

    pub fn known(&self) -> Option<&T> {
        match self {
            Attr::Known { value, .. } => Some(value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildSystem {
    Cargo,
    Npm,
    Pnpm,
    Yarn,
    Bun,
    Python,
    Go,
    Maven,
    Gradle,
}

impl BuildSystem {
    pub fn name(self) -> &'static str {
        match self {
            BuildSystem::Cargo => "cargo",
            BuildSystem::Npm => "npm",
            BuildSystem::Pnpm => "pnpm",
            BuildSystem::Yarn => "yarn",
            BuildSystem::Bun => "bun",
            BuildSystem::Python => "python",
            BuildSystem::Go => "go",
            BuildSystem::Maven => "maven",
            BuildSystem::Gradle => "gradle",
        }
    }

    fn language(self) -> &'static str {
        match self {
            BuildSystem::Cargo => "rust",
            BuildSystem::Npm | BuildSystem::Pnpm | BuildSystem::Yarn | BuildSystem::Bun => {
                "javascript"
            }
            BuildSystem::Python => "python",
            BuildSystem::Go => "go",
            BuildSystem::Maven | BuildSystem::Gradle => "java",
        }
    }
}

/// What can establish that the project's tests pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    /// PAX is present and the project is Cargo: PAX establishes `passed` from libtest evidence.
    PaxCargo,
    /// PAX is present but the project is not Cargo: PAX runs the tool and reports `unsupported`
    /// (it has no authoritative test semantics for it), so no test result can verify a change.
    PaxUninterpreted,
    /// PAX is not present: nothing in this binary can run the project's tests.
    None,
}

/// Facts the caller established before profiling, without executing anything new.
#[derive(Debug, Clone)]
pub struct ProfileInputs {
    pub root: PathBuf,
    /// Where `pax` resolved, or why it did not (`PaxExecutor::locate`: a `PATH` lookup).
    pub pax: Result<PathBuf, String>,
    /// Whether a `git` executable is on `PATH` (a lookup, not a run).
    pub git_on_path: bool,
    /// The selected model's provider and name, or why none is selected. Never a credential.
    pub model: Result<(String, String), String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelAvailability {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectProfile {
    pub version: &'static str,
    /// Every observation, sorted. Attributes cite them by `source`.
    pub observations: Vec<Observation>,
    /// Build systems whose manifests are at the root (observed), sorted.
    pub build_systems: Vec<BuildSystem>,
    pub primary_build_system: Attr<BuildSystem>,
    pub languages: Attr<Vec<&'static str>>,
    pub has_tests: Attr<bool>,
    pub verification: Attr<Verification>,
    pub git_repository: Attr<bool>,
    pub git_tool: Attr<bool>,
    pub pax_tool: Attr<bool>,
    pub model: Attr<ModelAvailability>,
    /// Never probed: profiling makes no network request.
    pub network: Attr<bool>,
}

/// Root manifests and the build system each indicates.
const MANIFESTS: &[(&str, BuildSystem)] = &[
    ("Cargo.toml", BuildSystem::Cargo),
    ("package-lock.json", BuildSystem::Npm),
    ("pnpm-lock.yaml", BuildSystem::Pnpm),
    ("yarn.lock", BuildSystem::Yarn),
    ("bun.lock", BuildSystem::Bun),
    ("bun.lockb", BuildSystem::Bun),
    ("pyproject.toml", BuildSystem::Python),
    ("setup.py", BuildSystem::Python),
    ("requirements.txt", BuildSystem::Python),
    ("go.mod", BuildSystem::Go),
    ("pom.xml", BuildSystem::Maven),
    ("build.gradle", BuildSystem::Gradle),
    ("build.gradle.kts", BuildSystem::Gradle),
];

impl ProjectProfile {
    pub fn collect(inputs: &ProfileInputs) -> Self {
        let root = &inputs.root;
        let mut obs: BTreeSet<Observation> = BTreeSet::new();
        let mut note = |source: String, fact: &str| {
            obs.insert(Observation {
                source,
                fact: fact.to_string(),
            });
        };

        let mut systems: BTreeSet<BuildSystem> = BTreeSet::new();
        let mut system_sources: Vec<String> = Vec::new();
        for (file, system) in MANIFESTS {
            if root.join(file).is_file() {
                note(
                    format!("file:{file}"),
                    &format!("present (indicates {})", system.name()),
                );
                systems.insert(*system);
                system_sources.push(format!("file:{file}"));
            }
        }
        // A package.json with no lockfile is npm by default, as PAX selects it.
        if root.join("package.json").is_file() {
            note("file:package.json".into(), "present");
            system_sources.push("file:package.json".into());
            if !systems.iter().any(|s| {
                matches!(
                    s,
                    BuildSystem::Npm | BuildSystem::Pnpm | BuildSystem::Yarn | BuildSystem::Bun
                )
            }) {
                systems.insert(BuildSystem::Npm);
            }
        }
        let build_systems: Vec<BuildSystem> = systems.iter().copied().collect();
        system_sources.sort();
        system_sources.dedup();
        let primary_build_system = match build_systems.as_slice() {
            [] => Attr::Unknown {
                reason: "no recognized build manifest at the project root".into(),
            },
            [one] => Attr::Known {
                value: *one,
                basis: Basis::Inferred {
                    rule: "the only build system with a manifest at the root",
                },
                evidence: system_sources.clone(),
            },
            many => Attr::Ambiguous {
                candidates: many.to_vec(),
                evidence: system_sources.clone(),
            },
        };
        let languages = if build_systems.is_empty() {
            Attr::Unknown {
                reason: "no build manifest to infer a language from".into(),
            }
        } else {
            let mut langs: Vec<&'static str> = build_systems.iter().map(|b| b.language()).collect();
            langs.sort();
            langs.dedup();
            Attr::Known {
                value: langs,
                basis: Basis::Inferred {
                    rule: "the language of each build system present",
                },
                evidence: system_sources.clone(),
            }
        };

        let has_tests = test_layout(root, &mut note);

        let git_dir = root.join(".git").exists();
        note(
            "dir:.git".into(),
            if git_dir { "present" } else { "absent" },
        );
        let git_repository = Attr::Known {
            value: git_dir,
            basis: Basis::Observed,
            evidence: vec!["dir:.git".into()],
        };
        note(
            "path:git".into(),
            if inputs.git_on_path {
                "found on PATH"
            } else {
                "not found on PATH"
            },
        );
        let git_tool = Attr::Known {
            value: inputs.git_on_path,
            basis: Basis::Observed,
            evidence: vec!["path:git".into()],
        };

        let pax_present = inputs.pax.is_ok();
        note(
            "path:pax".into(),
            if pax_present {
                "resolved"
            } else {
                "not resolved"
            },
        );
        let pax_tool = Attr::Known {
            value: pax_present,
            basis: Basis::Observed,
            evidence: vec!["path:pax".into()],
        };

        let mut verification_evidence = vec!["path:pax".to_string()];
        verification_evidence.extend(system_sources.iter().cloned());
        let verification = match (&primary_build_system, pax_present) {
            (_, false) => Attr::Known {
                value: Verification::None,
                basis: Basis::Inferred {
                    rule: "PAX is the only test verifier and it is not present",
                },
                evidence: vec!["path:pax".into()],
            },
            (
                Attr::Known {
                    value: BuildSystem::Cargo,
                    ..
                },
                true,
            ) => Attr::Known {
                value: Verification::PaxCargo,
                basis: Basis::Inferred {
                    rule: "PAX establishes test results from libtest evidence for Cargo projects",
                },
                evidence: verification_evidence,
            },
            (Attr::Known { .. }, true) => Attr::Known {
                value: Verification::PaxUninterpreted,
                basis: Basis::Inferred {
                    rule: "PAX has no authoritative test semantics for this build system and reports unsupported",
                },
                evidence: verification_evidence,
            },
            (Attr::Ambiguous { .. }, true) => Attr::Unknown {
                reason:
                    "several build systems are present; PAX's own selection decides which tool runs"
                        .into(),
            },
            (Attr::Unknown { .. }, true) => Attr::Unknown {
                reason: "no build system was recognized; PAX's own detection decides".into(),
            },
        };

        let model = match &inputs.model {
            Ok((provider, model)) => {
                note("runtime:model".into(), "a model is selected");
                Attr::Known {
                    value: ModelAvailability {
                        provider: provider.clone(),
                        model: model.clone(),
                    },
                    basis: Basis::Observed,
                    evidence: vec!["runtime:model".into()],
                }
            }
            Err(why) => {
                note("runtime:model".into(), "no model is selected");
                Attr::Unknown {
                    reason: format!("no model is selected ({why})"),
                }
            }
        };

        ProjectProfile {
            version: PROFILE_VERSION,
            observations: obs.into_iter().collect(),
            build_systems,
            primary_build_system,
            languages,
            has_tests,
            verification,
            git_repository,
            git_tool,
            pax_tool,
            model,
            network: Attr::Unknown {
                reason: "not probed: profiling makes no network request".into(),
            },
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("a profile serializes")
    }

    /// `sha256:` of the canonical profile.
    pub fn digest(&self) -> String {
        format!(
            "sha256:{}",
            Sha256::digest(self.to_json().to_string().as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        )
    }
}

/// Whether the project has tests, from its layout only: a `tests`, `test` or `__tests__`
/// directory, or files named like tests. File contents are not read.
fn test_layout(root: &Path, note: &mut impl FnMut(String, &str)) -> Attr<bool> {
    let mut stack = vec![PathBuf::new()];
    let mut scanned = 0usize;
    let mut found: Vec<String> = Vec::new();
    while let Some(rel) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(root.join(&rel)) else {
            continue;
        };
        let mut names: Vec<(String, bool)> = entries
            .flatten()
            .filter_map(|e| {
                let kind = e.file_type().ok()?;
                (!kind.is_symlink())
                    .then(|| (e.file_name().to_string_lossy().into_owned(), kind.is_dir()))
            })
            .collect();
        names.sort();
        for (name, is_dir) in names {
            scanned += 1;
            if scanned > MAX_SCANNED {
                return Attr::Unknown {
                    reason: format!(
                        "more than {MAX_SCANNED} entries; the layout was not fully examined"
                    ),
                };
            }
            if matches!(name.as_str(), ".git" | "target" | "node_modules" | ".chip")
                || name.starts_with('.')
            {
                continue;
            }
            let path = if rel.as_os_str().is_empty() {
                PathBuf::from(&name)
            } else {
                rel.join(&name)
            };
            let shown = path.to_string_lossy().replace('\\', "/");
            if is_dir {
                if matches!(name.as_str(), "tests" | "test" | "__tests__") {
                    found.push(format!("dir:{shown}"));
                }
                stack.push(path);
            } else {
                let stem = name.split('.').next().unwrap_or_default();
                if stem.ends_with("_test")
                    || stem.ends_with("_tests")
                    || stem.starts_with("test_")
                    || name.contains(".test.")
                    || name.contains(".spec.")
                {
                    found.push(format!("file:{shown}"));
                }
            }
        }
    }
    found.sort();
    found.truncate(5);
    if found.is_empty() {
        note(
            "layout:tests".into(),
            "no test directory or test-named file found",
        );
        Attr::Known {
            value: false,
            basis: Basis::Inferred {
                rule: "no test directory or test-named file in the examined layout (tests inline in source files are not seen)",
            },
            evidence: vec!["layout:tests".into()],
        }
    } else {
        for f in &found {
            note(f.clone(), "test layout");
        }
        Attr::Known {
            value: true,
            basis: Basis::Observed,
            evidence: found,
        }
    }
}

/// Whether an executable named `name` is on `PATH`. A lookup only; nothing is run.
pub fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(name);
            candidate.is_file()
        })
    })
}
