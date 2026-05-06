//! Prompt templates: markdown files with `{{var}}` placeholders.
//!
//! Templates are stored as `<name>.md` files (FLAT, not per-directory like
//! Skills) under one of `~/.hand/templates/`, `<cwd>/.hand/templates/`, or
//! a builtin path. Frontmatter is optional and carries metadata (display
//! name, description, expected variables); the body is the template text.
//!
//! `Template::render(vars)` substitutes `{{var}}` placeholders. Missing
//! variables produce a `TemplateError::MissingVariable`.

use crate::core::resource_loader::{
    LayoutKind, NameResolver, ResourceLoaderConfig, ResourceLoaderError, discover_resources_lenient,
};
use crate::core::source_info::{SourceInfo, SourceScope};
use regex_lite::Regex;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use thiserror::Error;

/// Parsed YAML frontmatter on a prompt template.
#[derive(Debug, Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
struct TemplateMetadata {
    /// Optional human-readable description.
    description: Option<String>,
    /// Optional list of expected variable names (used for validation/help).
    variables: Vec<String>,
}

/// A discovered prompt template.
#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    /// Canonical name (file stem of the .md file).
    pub name: String,
    /// Optional description from frontmatter.
    pub description: Option<String>,
    /// Variable names declared in frontmatter (informational; render-time
    /// missing-variable errors are based on what's actually in the body).
    pub declared_variables: Vec<String>,
    /// Template body (post-frontmatter).
    pub body: String,
    /// Where it came from.
    pub source: SourceInfo,
}

/// Errors raised while loading or rendering a prompt template.
#[derive(Debug, Error)]
pub enum TemplateError {
    /// A `{{var}}` placeholder in the body has no value in the supplied vars map.
    #[error("template {template_name:?} references undefined variable {variable:?}")]
    MissingVariable {
        template_name: String,
        variable: String,
    },
    /// IO or frontmatter error from the loader.
    #[error("loader error in {path}: {source}")]
    Loader {
        path: PathBuf,
        #[source]
        source: ResourceLoaderError,
    },
}

/// Compiled placeholder regex: `{{<ident>}}` with no whitespace inside the
/// braces. The identifier shape is the Rust identifier shape.
fn placeholder_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{\{([A-Za-z_][A-Za-z0-9_]*)\}\}").unwrap())
}

impl Template {
    /// Render the template body, substituting `{{var}}` for values from `vars`.
    /// Returns `Err(MissingVariable)` on the first undefined reference.
    ///
    /// All placeholders are validated against `vars` before substitution
    /// begins, so the FIRST missing variable in source order is reported.
    /// Surplus entries in `vars` (not referenced by the body) are ignored.
    pub fn render(&self, vars: &HashMap<&str, &str>) -> Result<String, TemplateError> {
        let re = placeholder_regex();

        // Validate first: report the earliest undefined reference, even when
        // a later one would also fail.
        for caps in re.captures_iter(&self.body) {
            let name = caps.get(1).expect("group 1 always present").as_str();
            if !vars.contains_key(name) {
                return Err(TemplateError::MissingVariable {
                    template_name: self.name.clone(),
                    variable: name.to_string(),
                });
            }
        }

        // All placeholders resolve; substitute.
        let rendered = re.replace_all(&self.body, |caps: &regex_lite::Captures<'_>| {
            let name = caps.get(1).expect("group 1 always present").as_str();
            // Already validated above.
            (*vars.get(name).expect("validated")).to_string()
        });

        Ok(rendered.into_owned())
    }
}

/// Discover prompt templates in builtin / user / project scopes.
///
/// Each provided directory is scanned non-recursively for `<name>.md` files.
/// Higher-priority scopes (project > user > builtin) shadow lower-priority
/// ones with the same canonical name. The result is sorted by name.
///
/// Per-file IO or frontmatter errors are collected as `TemplateError` and
/// returned alongside the successful templates. A missing directory is not
/// an error.
pub fn discover_templates(
    cwd: &Path,
    user_dir: Option<&Path>,
    builtin_dir: Option<&Path>,
) -> (Vec<Template>, Vec<TemplateError>) {
    let mut roots: Vec<(PathBuf, SourceScope)> = Vec::new();
    if let Some(builtin) = builtin_dir {
        roots.push((builtin.to_path_buf(), SourceScope::Builtin));
    }
    if let Some(user) = user_dir {
        roots.push((user.to_path_buf(), SourceScope::User));
    }
    roots.push((cwd.join(".hand").join("templates"), SourceScope::Project));

    let cfg = ResourceLoaderConfig {
        roots,
        resource_filename: "",
        name_resolver: NameResolver::FileStem,
        layout: LayoutKind::Flat,
    };

    let (raws, failures) = discover_resources_lenient::<TemplateMetadata>(&cfg);

    let templates: Vec<Template> = raws
        .into_iter()
        .map(|r| {
            let meta = r.metadata.unwrap_or_default();
            Template {
                name: r.name,
                description: meta.description,
                declared_variables: meta.variables,
                body: r.body,
                source: r.source,
            }
        })
        .collect();

    let errors: Vec<TemplateError> = failures
        .into_iter()
        .map(|(path, source)| TemplateError::Loader { path, source })
        .collect();

    (templates, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn vars<'a>(pairs: &'a [(&'a str, &'a str)]) -> HashMap<&'a str, &'a str> {
        pairs.iter().copied().collect()
    }

    fn template(body: &str) -> Template {
        Template {
            name: "tpl".to_string(),
            description: None,
            declared_variables: Vec::new(),
            body: body.to_string(),
            source: SourceInfo::builtin("/tmp/tpl.md"),
        }
    }

    // 1. Basic substitution.
    #[test]
    fn renders_single_placeholder() {
        let t = template("Hello {{name}}!");
        let v = vars(&[("name", "world")]);
        assert_eq!(t.render(&v).unwrap(), "Hello world!");
    }

    // 2. Multiple substitutions.
    #[test]
    fn renders_multiple_placeholders() {
        let t = template("{{a}} and {{b}}");
        let v = vars(&[("a", "x"), ("b", "y")]);
        assert_eq!(t.render(&v).unwrap(), "x and y");
    }

    // 3. Repeated variable.
    #[test]
    fn renders_repeated_variable() {
        let t = template("{{x}}{{x}}");
        let v = vars(&[("x", "yo")]);
        assert_eq!(t.render(&v).unwrap(), "yoyo");
    }

    // 4. Missing variable.
    #[test]
    fn errors_on_missing_variable() {
        let t = template("{{undefined}}");
        let err = t.render(&vars(&[])).unwrap_err();
        match err {
            TemplateError::MissingVariable {
                template_name,
                variable,
            } => {
                assert_eq!(template_name, "tpl");
                assert_eq!(variable, "undefined");
            }
            other => panic!("expected MissingVariable, got {other:?}"),
        }
    }

    // 5. Empty body.
    #[test]
    fn renders_empty_body() {
        let t = template("");
        assert_eq!(t.render(&vars(&[])).unwrap(), "");
    }

    // 6. Body without placeholders.
    #[test]
    fn renders_body_without_placeholders() {
        let t = template("hello world");
        assert_eq!(t.render(&vars(&[])).unwrap(), "hello world");
    }

    // 7. Adjacent placeholders.
    #[test]
    fn renders_adjacent_placeholders() {
        let t = template("{{a}}{{b}}");
        let v = vars(&[("a", "1"), ("b", "2")]);
        assert_eq!(t.render(&v).unwrap(), "12");
    }

    // 8. Variable name with underscore.
    #[test]
    fn renders_variable_with_underscore() {
        let t = template("{{my_var}}");
        let v = vars(&[("my_var", "z")]);
        assert_eq!(t.render(&v).unwrap(), "z");
    }

    // 9. Discovery test: two templates under a project dir.
    #[test]
    fn discover_finds_flat_templates_sorted() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let templates_dir = project.join(".hand").join("templates");
        fs::create_dir_all(&templates_dir).unwrap();
        fs::write(
            templates_dir.join("zeta.md"),
            "---\ndescription: zeta desc\n---\nhello {{name}}",
        )
        .unwrap();
        fs::write(
            templates_dir.join("alpha.md"),
            "---\ndescription: alpha desc\n---\nbody",
        )
        .unwrap();

        let (found, errors) = discover_templates(project, None, None);

        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let names: Vec<_> = found.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
        assert_eq!(found[0].description.as_deref(), Some("alpha desc"));
        assert_eq!(found[1].description.as_deref(), Some("zeta desc"));
        assert_eq!(found[1].body, "hello {{name}}");
    }

    // 10. Whitespace inside braces is NOT a placeholder — left literal.
    //
    // Choice: `{{ name }}` (with internal spaces) is treated as literal text,
    // not a placeholder. This matches the strict regex
    // `\{\{<ident>\}\}` and avoids surprising interpretations. Documented
    // here so the behavior is intentional.
    #[test]
    fn whitespace_inside_braces_is_literal() {
        let t = template("Hello {{ name }}!");
        // `name` is unused; rendering with no vars must succeed and preserve
        // the literal sequence.
        assert_eq!(t.render(&vars(&[])).unwrap(), "Hello {{ name }}!");
    }

    // Extra: surplus vars are ignored.
    #[test]
    fn extra_vars_are_ignored() {
        let t = template("hi {{a}}");
        let v = vars(&[("a", "1"), ("b", "2"), ("c", "3")]);
        assert_eq!(t.render(&v).unwrap(), "hi 1");
    }

    // Extra: first missing variable wins (left-to-right).
    #[test]
    fn first_missing_variable_is_reported() {
        let t = template("{{a}} {{b}} {{c}}");
        let v = vars(&[("c", "ok")]);
        let err = t.render(&v).unwrap_err();
        match err {
            TemplateError::MissingVariable { variable, .. } => {
                assert_eq!(variable, "a");
            }
            other => panic!("expected MissingVariable, got {other:?}"),
        }
    }

    // Extra: a placeholder whose name starts with a digit is not recognised
    // (Rust-identifier shape only).
    #[test]
    fn digit_leading_name_not_a_placeholder() {
        let t = template("{{1bad}}");
        // Treated literally; render succeeds with no vars.
        assert_eq!(t.render(&vars(&[])).unwrap(), "{{1bad}}");
    }

    // Extra: discovery picks project over user over builtin for same name.
    #[test]
    fn discover_project_shadows_user_and_builtin() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let user_dir = tmp.path().join("user-templates");
        let builtin_dir = tmp.path().join("builtin-templates");
        let project_dir = project.join(".hand").join("templates");
        fs::create_dir_all(&project_dir).unwrap();
        fs::create_dir_all(&user_dir).unwrap();
        fs::create_dir_all(&builtin_dir).unwrap();

        fs::write(builtin_dir.join("greet.md"), "from builtin").unwrap();
        fs::write(user_dir.join("greet.md"), "from user").unwrap();
        fs::write(project_dir.join("greet.md"), "from project").unwrap();

        let (found, errors) =
            discover_templates(project, Some(&user_dir), Some(&builtin_dir));

        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "greet");
        assert_eq!(found[0].body, "from project");
        assert_eq!(found[0].source.scope, SourceScope::Project);
    }
}
