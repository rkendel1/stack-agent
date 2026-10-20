//! The project path policy: which names a model may never address, and which paths it may read but
//! never write.
//!
//! Two classes, kept apart on purpose:
//!
//! * **Withheld** names are invisible to every project capability. A model cannot read, write, list
//!   or search them, and a listing or search never shows them. They are the host's own state
//!   (`.git`, `.stack-agent`, Chip's temporary files) and files whose content is a credential
//!   (`.env*`, `.aws`, `.ssh`, private keys, `secrets.toml`, Terraform state, and so on). File
//!   content goes to the model's provider, so a credential that can be read can leave the machine.
//!   A withheld decision depends on one path component alone, so a path, a listing and a search
//!   agree on what is hidden.
//! * **Protected** paths may be read but never written. They are the surfaces that run code, or
//!   steer an agent, without the verification a work performs: a Cargo build script and Cargo's own
//!   configuration (run by the editor's language server as soon as the project is opened, and by
//!   any later build), the toolchain file, CI pipelines, Git hook managers, agent instruction files,
//!   editor tasks that run on open, and shell start-up files. Writing ordinary source and running
//!   the project's tests is still code execution through the project's own tooling; this policy
//!   does not change that, and it is not a sandbox. It keeps a model, or a file that has talked one
//!   into it, from planting code or instructions that run outside the work.
//!
//! Every comparison folds ASCII case and ignores trailing dots, because the default macOS and
//! Windows filesystems treat `.ENV`, `.Git` and `.git.` as `.env` and `.git`.
//!
//! The lists follow the credential-access and sensitive-edit rules of Beacon's open threat-rule
//! corpus (`rules/credential-access`, `rules/sensitive-edit` in asymptote-labs/agent-beacon, MIT),
//! narrowed to what a coding agent working in one project directory can reach. The fixtures in
//! `tests/fixtures/path-policy.json` pin every rule.

/// One policy rule, as the refusal reports it. The id is stable; the description says why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    pub id: &'static str,
    pub description: &'static str,
}

const HOST_STATE: Rule = Rule {
    id: "host-state",
    description: "Chip's own state: the Git directory, stack-agent's records and temporary files",
};
const ENV_FILE: Rule = Rule {
    id: "env-file",
    description: "environment files hold credentials",
};
const CREDENTIAL_STORE: Rule = Rule {
    id: "credential-store",
    description: "a directory of cloud, SSH, GPG, container or cluster credentials",
};
const CREDENTIAL_FILE: Rule = Rule {
    id: "credential-file",
    description: "a file whose content is a credential",
};
const PRIVATE_KEY: Rule = Rule {
    id: "private-key",
    description: "a private key or key store",
};
const SECRETS_DATA: Rule = Rule {
    id: "secrets-data",
    description: "a data file named for secrets or credentials",
};
const INFRA_STATE: Rule = Rule {
    id: "infrastructure-state",
    description: "Terraform state holds every secret it provisioned",
};

pub const CARGO_BUILD_SCRIPT: Rule = Rule {
    id: "cargo-build-script",
    description: "a Cargo build script runs on every build and when an editor opens the project",
};
pub const CARGO_CONFIG: Rule = Rule {
    id: "cargo-config",
    description: "Cargo configuration can set a compiler wrapper, runner or linker that runs as you",
};
pub const CARGO_MANIFEST_BUILD: Rule = Rule {
    id: "cargo-manifest-build",
    description: "changing a manifest's build-script setting names new code to run on every build",
};
const RUST_TOOLCHAIN: Rule = Rule {
    id: "rust-toolchain",
    description: "the toolchain file decides which compiler every build runs",
};
const CI_PIPELINE: Rule = Rule {
    id: "ci-pipeline",
    description: "a CI pipeline runs with the repository's CI credentials",
};
const GIT_HOOKS: Rule = Rule {
    id: "git-hooks",
    description: "Git hook managers run on commit and push",
};
const AGENT_INSTRUCTIONS: Rule = Rule {
    id: "agent-instructions",
    description: "agent instruction files steer every future agent in this project",
};
const EDITOR_AUTORUN: Rule = Rule {
    id: "editor-autorun",
    description: "editor and dev-container settings can run tasks when the project is opened",
};
const SHELL_INIT: Rule = Rule {
    id: "shell-init",
    description: "shell start-up files run in every new shell",
};

/// Every rule, withheld then protected. The fixtures must exercise each one.
pub const ALL: &[Rule] = &[
    HOST_STATE,
    ENV_FILE,
    CREDENTIAL_STORE,
    CREDENTIAL_FILE,
    PRIVATE_KEY,
    SECRETS_DATA,
    INFRA_STATE,
    CARGO_BUILD_SCRIPT,
    CARGO_CONFIG,
    CARGO_MANIFEST_BUILD,
    RUST_TOOLCHAIN,
    CI_PIPELINE,
    GIT_HOOKS,
    AGENT_INSTRUCTIONS,
    EDITOR_AUTORUN,
    SHELL_INIT,
];

/// A component as the policy compares it: ASCII lower case, trailing dots removed.
fn fold(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

const HOST_NAMES: &[&str] = &[".git", ".stack-agent"];
const CREDENTIAL_DIRS: &[&str] = &[
    ".aws",
    ".ssh",
    ".gnupg",
    ".docker",
    ".kube",
    ".azure",
    ".password-store",
];
const CREDENTIAL_NAMES: &[&str] = &[
    ".netrc",
    "_netrc",
    ".pgpass",
    ".git-credentials",
    ".npmrc",
    ".pypirc",
    ".htpasswd",
    ".vault-token",
    ".terraformrc",
    "terraform.rc",
    "kubeconfig",
];
const PRIVATE_KEY_NAMES: &[&str] = &[
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ecdsa_sk",
    "id_ed25519",
    "id_ed25519_sk",
];
const PRIVATE_KEY_EXTENSIONS: &[&str] = &[
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".jks",
    ".keystore",
    ".ppk",
    ".kdbx",
];
const SECRET_STEMS: &[&str] = &[
    "secret",
    "secrets",
    "credential",
    "credentials",
    "service-account",
    "service_account",
];
/// A secrets-named file is withheld only as data, so `src/credentials.rs` stays readable code.
const DATA_EXTENSIONS: &[&str] = &[
    "",
    "json",
    "toml",
    "yaml",
    "yml",
    "ini",
    "txt",
    "env",
    "cfg",
    "conf",
    "xml",
    "csv",
    "properties",
];

/// Why a model may never address a path component, or `None` when it may. The decision depends on
/// this one component, wherever it sits in a path.
pub fn withheld(component: &str) -> Option<Rule> {
    let name = fold(component);
    let name = name.as_str();
    if HOST_NAMES.contains(&name) || name.starts_with(".chip-write-") {
        return Some(HOST_STATE);
    }
    if name.starts_with(".env") {
        return Some(ENV_FILE);
    }
    if CREDENTIAL_DIRS.contains(&name) {
        return Some(CREDENTIAL_STORE);
    }
    if CREDENTIAL_NAMES.contains(&name) {
        return Some(CREDENTIAL_FILE);
    }
    if PRIVATE_KEY_NAMES.contains(&name) || PRIVATE_KEY_EXTENSIONS.iter().any(|e| name.ends_with(e))
    {
        return Some(PRIVATE_KEY);
    }
    if name.contains(".tfstate") {
        return Some(INFRA_STATE);
    }
    let (stem, extension) = name.split_once('.').unwrap_or((name, ""));
    if SECRET_STEMS.contains(&stem) && DATA_EXTENSIONS.contains(&extension) {
        return Some(SECRETS_DATA);
    }
    None
}

const CI_NAMES: &[&str] = &[
    ".gitlab-ci.yml",
    ".travis.yml",
    "azure-pipelines.yml",
    "bitbucket-pipelines.yml",
    "jenkinsfile",
    ".drone.yml",
];
const CI_DIRS: &[&str] = &[".circleci", ".buildkite"];
const GIT_HOOK_NAMES: &[&str] = &[
    ".pre-commit-config.yaml",
    "lefthook.yml",
    "lefthook.yaml",
    ".lefthook.yml",
];
const GIT_HOOK_DIRS: &[&str] = &[".husky", ".githooks"];
const AGENT_NAMES: &[&str] = &[
    "agents.md",
    "claude.md",
    "gemini.md",
    ".cursorrules",
    ".windsurfrules",
    ".clinerules",
    "copilot-instructions.md",
];
const AGENT_DIRS: &[&str] = &[".claude", ".cursor", ".gemini", ".kiro", ".clinerules"];
const EDITOR_DIRS: &[&str] = &[".vscode", ".idea", ".devcontainer"];
const SHELL_NAMES: &[&str] = &[
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".bash_logout",
    ".profile",
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".zlogin",
];

/// Why a model may read but never write `parts` (an already lexically valid path), or `None` when
/// it may write it. A manifest is not protected as a whole: [`manifest_build_change`] decides a
/// `Cargo.toml` write from its content.
pub fn protected(parts: &[&str]) -> Option<Rule> {
    let folded: Vec<String> = parts.iter().map(|p| fold(p)).collect();
    let name = folded.last().map(String::as_str).unwrap_or_default();
    let dirs = &folded[..folded.len().saturating_sub(1)];
    let under = |set: &[&str]| dirs.iter().any(|d| set.contains(&d.as_str()));
    if name == "build.rs" {
        return Some(CARGO_BUILD_SCRIPT);
    }
    if under(&[".cargo"]) {
        return Some(CARGO_CONFIG);
    }
    if name == "rust-toolchain" || name == "rust-toolchain.toml" {
        return Some(RUST_TOOLCHAIN);
    }
    let github = dirs
        .windows(2)
        .any(|w| w[0] == ".github" && matches!(w[1].as_str(), "workflows" | "actions"));
    if github || under(CI_DIRS) || CI_NAMES.contains(&name) {
        return Some(CI_PIPELINE);
    }
    if under(GIT_HOOK_DIRS) || GIT_HOOK_NAMES.contains(&name) {
        return Some(GIT_HOOKS);
    }
    if under(AGENT_DIRS) || AGENT_NAMES.contains(&name) {
        return Some(AGENT_INSTRUCTIONS);
    }
    if under(EDITOR_DIRS) {
        return Some(EDITOR_AUTORUN);
    }
    if SHELL_NAMES.contains(&name) {
        return Some(SHELL_INIT);
    }
    None
}

/// Whether `parts` names a Cargo manifest, whose writes [`manifest_build_change`] checks.
pub fn is_manifest(parts: &[&str]) -> bool {
    parts.last().is_some_and(|n| fold(n) == "cargo.toml")
}

/// The lines of a manifest that choose a build script: `build = ...` (any value other than `false`
/// names a script; `false` turns the default `build.rs` off), normalised so formatting alone is not
/// a change. Read line by line, without parsing TOML: a `build` key appears only in `[package]`,
/// and a manifest that hides one in an inline table or a multi-line string is refused if the
/// normalised lines differ, which is the conservative direction.
fn build_settings(manifest: &str) -> Vec<String> {
    manifest
        .lines()
        .filter_map(|line| {
            let line = line.split('#').next().unwrap_or_default();
            let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
            let rest = compact
                .strip_prefix("build=")
                .or_else(|| compact.strip_prefix("\"build\"="))
                .or_else(|| compact.strip_prefix("'build'="))?;
            Some(rest.replace('\'', "\""))
        })
        .collect()
}

/// `Some(rule)` when writing `new` over a manifest that held `old` (`None` for a new file) changes
/// which build script runs. Adding, removing or editing dependencies, features, targets or any
/// other key is allowed; adding `build = false` to a manifest that had no build setting is allowed
/// too, because it only stops a script from running.
pub fn manifest_build_change(old: Option<&str>, new: &str) -> Option<Rule> {
    let before = old.map(build_settings).unwrap_or_default();
    let after = build_settings(new);
    if before == after {
        return None;
    }
    let only_disables = before.is_empty() && after.iter().all(|v| v == "false");
    (!only_disables).then_some(CARGO_MANIFEST_BUILD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_closes_the_case_and_trailing_dot_aliases() {
        for name in [
            ".git",
            ".GIT",
            ".Git",
            ".git.",
            ".ENV",
            ".Env.Local",
            ".AWS",
            "ID_RSA",
        ] {
            assert!(withheld(name).is_some(), "{name}");
        }
        assert!(withheld("src").is_none());
    }

    #[test]
    fn a_manifest_may_change_anything_but_its_build_script() {
        let base = "[package]\nname = \"x\"\n\n[dependencies]\n";
        assert_eq!(
            manifest_build_change(Some(base), &format!("{base}serde = \"1\"\n")),
            None
        );
        assert_eq!(
            manifest_build_change(
                Some(base),
                &base.replace("name", "build = \"gen.rs\"\nname")
            ),
            Some(CARGO_MANIFEST_BUILD)
        );
        assert_eq!(
            manifest_build_change(None, "[package]\nbuild = 'x.rs'\n"),
            Some(CARGO_MANIFEST_BUILD)
        );
        assert_eq!(
            manifest_build_change(Some(base), &base.replace("name", "build = false\nname")),
            None,
            "turning the default build script off is allowed"
        );
        let with = "[package]\nbuild = \"b.rs\"\n";
        assert_eq!(
            manifest_build_change(
                Some(with),
                "[package]\nbuild=\"b.rs\" # kept\n[dependencies]\n"
            ),
            None,
            "an unchanged setting, reformatted, is not a change"
        );
        assert_eq!(
            manifest_build_change(Some(with), "[package]\n"),
            Some(CARGO_MANIFEST_BUILD),
            "removing a setting re-enables the default build.rs"
        );
    }
}
