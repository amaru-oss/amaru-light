//! Prompt templates: built in, and overridable per installation.
//!
//! A file `<prompts dir>/<name>.md` replaces the built-in template of the
//! same name. Placeholders are written `{{name}}`.

use crate::contract;
use std::path::PathBuf;

const BUILT_IN: [(&str, &str); 8] = [
    ("continue", include_str!("../prompts/continue.md")),
    ("implement", include_str!("../prompts/implement.md")),
    ("fix_checks", include_str!("../prompts/fix_checks.md")),
    ("self_review", include_str!("../prompts/self_review.md")),
    (
        "address_self_review",
        include_str!("../prompts/address_self_review.md"),
    ),
    (
        "address_review",
        include_str!("../prompts/address_review.md"),
    ),
    ("context", include_str!("../prompts/context.md")),
    (
        "missing_result",
        include_str!("../prompts/missing_result.md"),
    ),
];

#[derive(Clone, Debug, Default)]
pub struct Prompts {
    overrides: Option<PathBuf>,
}

fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    vars.iter().fold(template.to_owned(), |text, (key, value)| {
        text.replace(&format!("{{{{{key}}}}}"), value)
    })
}

impl Prompts {
    pub const fn new(overrides: Option<PathBuf>) -> Self {
        Self { overrides }
    }

    fn template(&self, name: &str) -> String {
        let custom = self
            .overrides
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join(format!("{name}.md"))).ok());
        custom.unwrap_or_else(|| {
            BUILT_IN
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, text)| (*text).to_owned())
                .unwrap_or_default()
        })
    }

    /// A template with its placeholders filled in, without the contract.
    pub fn text(&self, name: &str, vars: &[(&str, &str)]) -> String {
        fill(&self.template(name), vars)
    }

    /// A full prompt: the filled template followed by the result contract.
    pub fn render(&self, name: &str, vars: &[(&str, &str)]) -> String {
        format!(
            "{}\n\n{}\n",
            self.text(name, vars).trim_end(),
            contract::INSTRUCTIONS
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_placeholders_and_appends_the_contract() {
        let prompts = Prompts::default();
        let text = prompts.render(
            "implement",
            &[("issue", "#7"), ("repo", "me/app"), ("title", "T")],
        );
        assert!(text.starts_with("You are working on issue #7 of me/app"));
        assert!(text.contains(".amaru/result.json"));
        for (name, _) in BUILT_IN {
            assert!(!prompts.text(name, &[]).is_empty(), "{name}");
        }
    }

    #[test]
    fn files_override_built_in_templates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("implement.md"), "Do {{title}}").unwrap();
        let prompts = Prompts::new(Some(dir.path().to_path_buf()));
        assert_eq!(prompts.text("implement", &[("title", "X")]), "Do X");
        assert!(prompts.text("fix_checks", &[]).contains("failed"));
    }
}
