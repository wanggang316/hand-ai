//! Skills are markdown files describing optional capabilities the model can use.
//!
//! A skill is a directory containing a SKILL.md file. The file's YAML
//! frontmatter declares metadata (name, description, disable-model-invocation,
//! ...); the body is the prose injected into the system prompt's "Skills"
//! section when the skill is enabled.
//!
//! Skills live in three scopes (Builtin / User / Project), in increasing
//! priority. Same-named skills in higher-priority scopes shadow lower ones.
//!
//! Discovery wraps [`crate::core::resource_loader`] with skill-specific
//! configuration. Per-file errors are collected separately from successes so
//! `--diagnostics` can surface them without aborting the session.

use crate::core::resource_loader::{
    DiscoveredResource, LayoutKind, NameResolver, ResourceLoaderConfig, ResourceLoaderError,
    discover_resources_lenient,
};
use crate::core::source_info::{SourceInfo, SourceScope};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Skill name length cap per the Agent Skills spec.
const MAX_NAME_LENGTH: usize = 64;
/// Description length cap per the Agent Skills spec.
const MAX_DESCRIPTION_LENGTH: usize = 1024;

/// Parsed YAML frontmatter on a SKILL.md.
///
/// Unknown fields are tolerated to match the TypeScript reference
/// (`SkillFrontmatter` in `pi-mono/packages/coding-agent/src/core/skills.ts`
/// uses an open object schema). Fixtures such as `unknown-field/` rely on
/// this lenient behaviour.
#[derive(Debug, Deserialize, Clone, Default, PartialEq)]
struct SkillMetadata {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "disable-model-invocation")]
    disable_model_invocation: bool,
}

/// A discovered, validated skill.
#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    /// Canonical name (lowercase ASCII alphanumeric + dashes). Derived from
    /// the parent directory; the frontmatter `name` field, when present,
    /// must equal it.
    pub name: String,
    /// Description shown to the model when listing available skills.
    pub description: String,
    /// Body of SKILL.md — everything after the frontmatter close.
    pub body: String,
    /// True when the model should NOT auto-invoke this skill (it can only be
    /// invoked explicitly, e.g., via `/skill:<name>`).
    pub disable_model_invocation: bool,
    /// Where the skill was discovered.
    pub source: SourceInfo,
}

/// Errors raised while validating a SKILL.md.
///
/// Wraps loader-level errors (`Loader`) plus skill-specific schema errors.
/// Collected per-file by [`discover_skills`] so the caller can surface them
/// without aborting the whole session.
#[derive(Debug, Error)]
pub enum SkillError {
    /// IO or frontmatter error from the underlying resource loader.
    #[error("loader error in {path}: {source}")]
    Loader {
        path: PathBuf,
        #[source]
        source: ResourceLoaderError,
    },
    /// SKILL.md frontmatter is missing the required `description` field.
    #[error("missing required field `description` in {path}")]
    MissingDescription { path: PathBuf },
    /// `description` exceeds the spec-mandated length cap.
    #[error("description exceeds {max} characters ({actual}) in {path}")]
    DescriptionTooLong {
        path: PathBuf,
        actual: usize,
        max: usize,
    },
    /// Frontmatter `name` doesn't match the directory name.
    #[error(
        "frontmatter `name` ({frontmatter_name:?}) doesn't match directory name ({dir_name:?}) at {path}"
    )]
    NameMismatch {
        path: PathBuf,
        frontmatter_name: String,
        dir_name: String,
    },
    /// Skill name fails validation (must be lowercase ASCII alphanumeric +
    /// dashes, no leading/trailing dash, no consecutive dashes, max 64 chars).
    #[error("invalid skill name {name:?} at {path}: {reason}")]
    InvalidName {
        path: PathBuf,
        name: String,
        reason: String,
    },
}

/// Discover SKILL.md files in builtin/user/project scopes.
///
/// Roots are scanned in scope-precedence order; same-named skills in higher
/// scopes shadow lower ones (project > user > builtin). `cwd` is the project
/// root — the loader looks under `<cwd>/.hand/skills/` for project skills.
/// `user_dir` typically points at `~/.hand/skills/` and `builtin_dir` at the
/// bundled defaults; either may be `None` to skip that scope.
///
/// Returns `(skills, errors)`. Per-file errors are collected; successful
/// skills are still returned. Callers (e.g., `--diagnostics`) can present
/// the errors but the main session is not aborted by a single bad skill.
pub fn discover_skills(
    cwd: &Path,
    user_dir: Option<&Path>,
    builtin_dir: Option<&Path>,
) -> (Vec<Skill>, Vec<SkillError>) {
    let project_dir = cwd.join(".hand").join("skills");

    let mut roots: Vec<(PathBuf, SourceScope)> = Vec::with_capacity(3);
    if let Some(builtin) = builtin_dir {
        roots.push((builtin.to_path_buf(), SourceScope::Builtin));
    }
    if let Some(user) = user_dir {
        roots.push((user.to_path_buf(), SourceScope::User));
    }
    roots.push((project_dir, SourceScope::Project));

    discover_skills_with_roots(roots)
}

/// Run the underlying loader against an explicit set of roots and validate
/// each entry. Internal entry point shared with tests that need direct
/// control over the root list.
fn discover_skills_with_roots(roots: Vec<(PathBuf, SourceScope)>) -> (Vec<Skill>, Vec<SkillError>) {
    let config = ResourceLoaderConfig {
        roots,
        resource_filename: "SKILL.md",
        name_resolver: NameResolver::ParentDirName,
        layout: LayoutKind::PerDirectory,
    };

    let (raw, loader_failures) = discover_resources_lenient::<SkillMetadata>(&config);

    let mut errors: Vec<SkillError> = loader_failures
        .into_iter()
        .map(|(path, source)| SkillError::Loader { path, source })
        .collect();

    let mut skills: Vec<Skill> = Vec::with_capacity(raw.len());
    for resource in raw {
        match validate(resource) {
            Ok(skill) => skills.push(skill),
            Err(err) => errors.push(err),
        }
    }

    (skills, errors)
}

/// Validate a discovered SKILL.md and turn it into a [`Skill`].
fn validate(resource: DiscoveredResource<SkillMetadata>) -> Result<Skill, SkillError> {
    let path = resource.source.path.clone();
    let dir_name = resource.name.clone();

    let metadata = resource.metadata.unwrap_or_default();

    // Description is required.
    let description = match metadata.description {
        Some(desc) if !desc.trim().is_empty() => desc,
        _ => return Err(SkillError::MissingDescription { path }),
    };

    if description.len() > MAX_DESCRIPTION_LENGTH {
        return Err(SkillError::DescriptionTooLong {
            path,
            actual: description.len(),
            max: MAX_DESCRIPTION_LENGTH,
        });
    }

    // If frontmatter name is provided, it must match the directory name.
    // Otherwise fall back to the directory name itself.
    let name = match metadata.name {
        Some(frontmatter_name) => {
            if frontmatter_name != dir_name {
                return Err(SkillError::NameMismatch {
                    path,
                    frontmatter_name,
                    dir_name,
                });
            }
            frontmatter_name
        }
        None => dir_name,
    };

    if let Err(reason) = validate_name(&name) {
        return Err(SkillError::InvalidName {
            path,
            name,
            reason: reason.to_string(),
        });
    }

    Ok(Skill {
        name,
        description,
        body: resource.body,
        disable_model_invocation: metadata.disable_model_invocation,
        source: resource.source,
    })
}

/// Validate a skill name per the Agent Skills spec.
///
/// Returns `Ok(())` if valid, `Err(reason)` otherwise.
fn validate_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("name must not be empty");
    }
    if name.len() > MAX_NAME_LENGTH {
        return Err("name exceeds 64 characters");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err("name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)");
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err("name must not start or end with a hyphen");
    }
    if name.contains("--") {
        return Err("name must not contain consecutive hyphens");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    /// Absolute path to the integration-test fixture corpus.
    fn fixtures_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("skills")
    }

    /// Stage a single fixture directory inside a fresh tempdir under a chosen
    /// scope-root layout, so each test's loader sees only that one skill.
    ///
    /// Returns the tempdir (kept alive by the caller) and the project root
    /// the loader should use as `cwd` (i.e., the tempdir; the loader looks
    /// for `.hand/skills` underneath it).
    fn stage_project_fixture(fixture: &str) -> TempDir {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join(".hand").join("skills").join(fixture);
        fs::create_dir_all(&dest).unwrap();
        let src_file = fixtures_root().join(fixture).join("SKILL.md");
        fs::copy(&src_file, dest.join("SKILL.md")).unwrap();
        tmp
    }

    fn discover_in(cwd: &Path) -> (Vec<Skill>, Vec<SkillError>) {
        discover_skills(cwd, None, None)
    }

    // 1. valid-skill: happy path.
    #[test]
    fn fixture_valid_skill_loads() {
        let tmp = stage_project_fixture("valid-skill");
        let (skills, errors) = discover_in(tmp.path());
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(skills.len(), 1);
        let s = &skills[0];
        assert_eq!(s.name, "valid-skill");
        assert_eq!(s.description, "A valid skill for testing purposes.");
        assert!(!s.disable_model_invocation);
        assert_eq!(s.source.scope, SourceScope::Project);
        assert!(s.body.contains("# Valid Skill"));
    }

    // 2. consecutive-hyphens: "bad--name" rejected.
    #[test]
    fn fixture_consecutive_hyphens_rejected() {
        let tmp = stage_project_fixture("consecutive-hyphens");
        let (skills, errors) = discover_in(tmp.path());
        assert!(skills.is_empty());
        assert_eq!(errors.len(), 1);
        // The directory is `consecutive-hyphens` and frontmatter `name` is
        // `bad--name`, so this trips NameMismatch first.
        assert!(
            matches!(errors[0], SkillError::NameMismatch { .. }),
            "unexpected: {:?}",
            errors[0]
        );
    }

    // 3. disable-model-invocation: boolean field is parsed.
    #[test]
    fn fixture_disable_model_invocation_loads() {
        let tmp = stage_project_fixture("disable-model-invocation");
        let (skills, errors) = discover_in(tmp.path());
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(skills.len(), 1);
        assert!(skills[0].disable_model_invocation);
    }

    // 4. invalid-name-chars: uppercase/underscore in name → name mismatch
    //    (frontmatter "Invalid_Name" vs dir "invalid-name-chars").
    #[test]
    fn fixture_invalid_name_chars_rejected() {
        let tmp = stage_project_fixture("invalid-name-chars");
        let (skills, errors) = discover_in(tmp.path());
        assert!(skills.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], SkillError::NameMismatch { .. }));
    }

    // 5. invalid-yaml: frontmatter parser errors out.
    #[test]
    fn fixture_invalid_yaml_rejected() {
        let tmp = stage_project_fixture("invalid-yaml");
        let (skills, errors) = discover_in(tmp.path());
        assert!(skills.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(
            matches!(
                errors[0],
                SkillError::Loader {
                    source: ResourceLoaderError::Frontmatter { .. },
                    ..
                }
            ),
            "unexpected: {:?}",
            errors[0]
        );
    }

    // 6. long-name: frontmatter name exceeds 64 chars.
    //    Directory is `long-name`, frontmatter is the >64-char name → caught
    //    as NameMismatch (the frontmatter name doesn't match the dir name).
    #[test]
    fn fixture_long_name_rejected() {
        let tmp = stage_project_fixture("long-name");
        let (skills, errors) = discover_in(tmp.path());
        assert!(skills.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], SkillError::NameMismatch { .. }));
    }

    // 7. missing-description: required field absent.
    #[test]
    fn fixture_missing_description_rejected() {
        let tmp = stage_project_fixture("missing-description");
        let (skills, errors) = discover_in(tmp.path());
        assert!(skills.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], SkillError::MissingDescription { .. }));
    }

    // 8. multiline-description: literal block `|` preserved as-is.
    #[test]
    fn fixture_multiline_description_loads() {
        let tmp = stage_project_fixture("multiline-description");
        let (skills, errors) = discover_in(tmp.path());
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(skills.len(), 1);
        // serde_yaml clip-chomps trailing newlines; the interior `\n`
        // separators are what matter.
        assert_eq!(
            skills[0].description,
            "This is a multiline description.\nIt spans multiple lines.\nAnd should be normalized.",
        );
    }

    // 9. name-mismatch: frontmatter name != directory name.
    #[test]
    fn fixture_name_mismatch_rejected() {
        let tmp = stage_project_fixture("name-mismatch");
        let (skills, errors) = discover_in(tmp.path());
        assert!(skills.is_empty());
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            SkillError::NameMismatch {
                frontmatter_name,
                dir_name,
                ..
            } => {
                assert_eq!(frontmatter_name, "different-name");
                assert_eq!(dir_name, "name-mismatch");
            }
            other => panic!("expected NameMismatch, got {other:?}"),
        }
    }

    // 10. nested/child-skill: nested SKILL.md not discovered (loader is
    //     non-recursive — `nested/` itself has no SKILL.md, so it's skipped).
    #[test]
    fn fixture_nested_child_not_discovered() {
        // Stage the entire `nested/` tree under .hand/skills/nested/.
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join(".hand").join("skills").join("nested");
        fs::create_dir_all(dest.join("child-skill")).unwrap();
        fs::copy(
            fixtures_root().join("nested/child-skill/SKILL.md"),
            dest.join("child-skill/SKILL.md"),
        )
        .unwrap();

        let (skills, errors) = discover_in(tmp.path());
        assert!(skills.is_empty(), "expected no top-level skill; got {skills:?}");
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    // 11. no-frontmatter: body without `---` envelope → MissingDescription.
    #[test]
    fn fixture_no_frontmatter_rejected() {
        let tmp = stage_project_fixture("no-frontmatter");
        let (skills, errors) = discover_in(tmp.path());
        assert!(skills.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], SkillError::MissingDescription { .. }));
    }

    // 12. root-skill-preferred: a SKILL.md at the root of the skill dir wins;
    //     nested-child/SKILL.md is not picked up by the non-recursive loader.
    #[test]
    fn fixture_root_skill_preferred() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp
            .path()
            .join(".hand")
            .join("skills")
            .join("root-skill-preferred");
        fs::create_dir_all(dest.join("nested-child")).unwrap();
        fs::copy(
            fixtures_root().join("root-skill-preferred/SKILL.md"),
            dest.join("SKILL.md"),
        )
        .unwrap();
        fs::copy(
            fixtures_root().join("root-skill-preferred/nested-child/SKILL.md"),
            dest.join("nested-child/SKILL.md"),
        )
        .unwrap();

        let (skills, errors) = discover_in(tmp.path());
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "root-skill-preferred");
        assert_eq!(skills[0].description, "Root skill should win.");
    }

    // 13. unknown-field: extra frontmatter keys are tolerated (TS parity).
    #[test]
    fn fixture_unknown_field_loads() {
        let tmp = stage_project_fixture("unknown-field");
        let (skills, errors) = discover_in(tmp.path());
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "unknown-field");
    }

    // 14. Name validation at the directory-name level: synthesise a project
    //     skill whose directory name itself violates the spec (consecutive
    //     dashes). No frontmatter `name`, so we hit InvalidName.
    #[test]
    fn directory_name_validation_invalid_chars() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join(".hand").join("skills").join("Bad_Name");
        fs::create_dir_all(&dest).unwrap();
        fs::write(
            dest.join("SKILL.md"),
            "---\ndescription: hi\n---\nbody",
        )
        .unwrap();

        let (skills, errors) = discover_in(tmp.path());
        assert!(skills.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(
            matches!(errors[0], SkillError::InvalidName { .. }),
            "unexpected: {:?}",
            errors[0]
        );
    }

    // 15. Description length cap.
    #[test]
    fn description_too_long_rejected() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join(".hand").join("skills").join("oversize");
        fs::create_dir_all(&dest).unwrap();
        let huge = "x".repeat(MAX_DESCRIPTION_LENGTH + 1);
        fs::write(
            dest.join("SKILL.md"),
            format!("---\ndescription: \"{huge}\"\n---\nbody"),
        )
        .unwrap();

        let (skills, errors) = discover_in(tmp.path());
        assert!(skills.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(
            matches!(errors[0], SkillError::DescriptionTooLong { .. }),
            "unexpected: {:?}",
            errors[0]
        );
    }

    // 16. Precedence: project shadows user.
    #[test]
    fn precedence_project_shadows_user() {
        let user = TempDir::new().unwrap();
        let project_root = TempDir::new().unwrap();
        let user_skill_dir = user.path().join("alpha");
        let project_skill_dir = project_root.path().join(".hand/skills/alpha");
        fs::create_dir_all(&user_skill_dir).unwrap();
        fs::create_dir_all(&project_skill_dir).unwrap();
        fs::write(
            user_skill_dir.join("SKILL.md"),
            "---\ndescription: from user\n---\nu body",
        )
        .unwrap();
        fs::write(
            project_skill_dir.join("SKILL.md"),
            "---\ndescription: from project\n---\np body",
        )
        .unwrap();

        let (skills, errors) = discover_skills(project_root.path(), Some(user.path()), None);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].source.scope, SourceScope::Project);
        assert_eq!(skills[0].description, "from project");
        assert_eq!(skills[0].body, "p body");
    }

    // 17. Precedence: user shadows builtin.
    #[test]
    fn precedence_user_shadows_builtin() {
        let builtin = TempDir::new().unwrap();
        let user = TempDir::new().unwrap();
        let project_root = TempDir::new().unwrap();
        for (root, label) in [(builtin.path(), "builtin"), (user.path(), "user")] {
            let dir = root.join("alpha");
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("SKILL.md"),
                format!("---\ndescription: from {label}\n---\n{label} body"),
            )
            .unwrap();
        }

        let (skills, errors) = discover_skills(
            project_root.path(),
            Some(user.path()),
            Some(builtin.path()),
        );
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].source.scope, SourceScope::User);
        assert_eq!(skills[0].description, "from user");
    }

    // 18. Precedence: project wins over user and builtin.
    #[test]
    fn precedence_project_wins_three_scopes() {
        let builtin = TempDir::new().unwrap();
        let user = TempDir::new().unwrap();
        let project_root = TempDir::new().unwrap();
        for (root, label) in [(builtin.path(), "builtin"), (user.path(), "user")] {
            let dir = root.join("alpha");
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("SKILL.md"),
                format!("---\ndescription: from {label}\n---\n{label} body"),
            )
            .unwrap();
        }
        let project_skill_dir = project_root.path().join(".hand/skills/alpha");
        fs::create_dir_all(&project_skill_dir).unwrap();
        fs::write(
            project_skill_dir.join("SKILL.md"),
            "---\ndescription: from project\n---\np body",
        )
        .unwrap();

        let (skills, errors) = discover_skills(
            project_root.path(),
            Some(user.path()),
            Some(builtin.path()),
        );
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].source.scope, SourceScope::Project);
        assert_eq!(skills[0].description, "from project");
    }

    // Sanity check: our internal name-validation cases.
    #[test]
    fn validate_name_table() {
        assert!(validate_name("valid-name").is_ok());
        assert!(validate_name("valid").is_ok());
        assert!(validate_name("v123").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("-leading").is_err());
        assert!(validate_name("trailing-").is_err());
        assert!(validate_name("with--double").is_err());
        assert!(validate_name("Upper").is_err());
        assert!(validate_name("under_score").is_err());
        assert!(validate_name(&"a".repeat(MAX_NAME_LENGTH)).is_ok());
        assert!(validate_name(&"a".repeat(MAX_NAME_LENGTH + 1)).is_err());
    }
}
