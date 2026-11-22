use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Settings {
    pub root_dir: PathBuf,
    pub project_path: PathBuf,
    pub project: Value,
    pub runtime: Value,
    pub providers: Value,
    pub models: Value,
    pub routing: Value,
}

pub fn expand_home(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if let Ok(rest) = path.strip_prefix("~")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    path.to_owned()
}

fn absolute(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = expand_home(path);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}

fn load_yaml(path: &Path) -> Result<Value> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("Cannot read configuration {}", path.display()))?;
    let value: Value = serde_yaml::from_str(&source)
        .with_context(|| format!("Invalid YAML in {}", path.display()))?;
    Ok(if value.is_null() {
        serde_json::json!({})
    } else {
        value
    })
}

impl Settings {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let project_path = absolute(path)?;
        let project_dir = project_path
            .parent()
            .context("Project config has no parent directory")?;
        let root_dir = project_dir
            .parent()
            .context("Project config must be inside projects/")?
            .to_owned();
        let mut project = load_yaml(&project_path)?;
        anyhow::ensure!(
            project["project"].is_object(),
            "Configuration requires a project mapping"
        );
        let project_root = expand_home(project["project"]["root"].as_str().unwrap_or("."));
        let project_root = absolute(if project_root.is_absolute() {
            project_root
        } else {
            project_dir.join(project_root)
        })?;
        project["project"]["root"] = Value::String(project_root.to_string_lossy().into_owned());
        let runtime = load_yaml(&root_dir.join("config/runtime.yaml"))?;
        let providers = load_yaml(&root_dir.join("config/providers.yaml"))?["providers"].clone();
        let models = load_yaml(&root_dir.join("config/models.yaml"))?["models"].clone();
        let routing = load_yaml(&root_dir.join("config/routing.yaml"))?["routing"].clone();
        Ok(Self {
            root_dir,
            project_path,
            project,
            runtime,
            providers,
            models,
            routing,
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.root_dir.join(
            self.runtime["storage"]["database"]
                .as_str()
                .unwrap_or("data/mini-amaru.db"),
        )
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.root_dir.join(
            self.runtime["storage"]["artifacts"]
                .as_str()
                .unwrap_or("data/artifacts"),
        )
    }

    pub fn project_root(&self) -> PathBuf {
        PathBuf::from(self.project["project"]["root"].as_str().unwrap_or("."))
    }

    pub fn project_name(&self) -> String {
        self.project["project"]["name"]
            .as_str()
            .unwrap_or("project")
            .to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_yaml_and_resolves_checkout_relative_to_project_file() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("projects"))?;
        std::fs::create_dir_all(dir.path().join("config"))?;
        std::fs::write(
            dir.path().join("projects/demo.yaml"),
            "project:\n  name: demo\n  root: ../repos/demo\n",
        )?;
        for (name, content) in [
            ("runtime", "storage:\n  database: data/test.db\n"),
            ("providers", "providers: {}\n"),
            ("models", "models: {}\n"),
            ("routing", "routing: {}\n"),
        ] {
            std::fs::write(dir.path().join(format!("config/{name}.yaml")), content)?;
        }
        let settings = Settings::load(dir.path().join("projects/demo.yaml"))?;
        assert_eq!(settings.project_name(), "demo");
        assert_eq!(
            settings.project_root(),
            dir.path().canonicalize()?.join("repos/demo")
        );
        assert_eq!(
            settings.db_path(),
            dir.path().canonicalize()?.join("data/test.db")
        );
        Ok(())
    }
}
