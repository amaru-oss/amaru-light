//! The guard's settings file. Every key is optional; an unknown key is an
//! error, as in the daemon's files.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The model the guard uses unless the settings name another.
pub const DEFAULT_MODEL: &str = "wolf-defender-prompt-injection-small";

/// Where a model's files come from, pinned to a revision and checked by
/// their SHA-256.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// The Hugging Face repository, `owner/name`.
    pub repository: String,
    /// The commit whose files are downloaded.
    pub revision: String,
    /// The ONNX graph, a path in the repository.
    pub model_file: String,
    pub model_sha256: String,
    /// The `tokenizer.json` of the model, a path in the repository.
    pub tokenizer_file: String,
    pub tokenizer_sha256: String,
    /// The index of the injection class in the model's logits.
    #[serde(default = "one")]
    pub injection_index: usize,
}

const fn one() -> usize {
    1
}

/// The Patronus Studio models the guard knows, in FP16, the precision whose
/// results the model cards publish next to FP32.
fn preset(name: &str) -> Option<Source> {
    let (repository, revision, model_sha256, tokenizer_sha256) = match name {
        "wolf-defender-prompt-injection-small" => (
            "patronus-studio/wolf-defender-prompt-injection-small",
            "142fadc5474163d4c483cb761d5a6b02e3aa1741",
            "f81eccd747b4fe9b077fb37c7ddbe0571c5b21e544e84fa95cc8bcbff9cd1332",
            "7e426c3929b44e6ab4c931770b5f22b913280633f5a1c67c81e9ad64decef55c",
        ),
        "wolf-defender-prompt-injection" => (
            "patronus-studio/wolf-defender-prompt-injection",
            "af0b16a1d40ca246595e48246ccf9f906a52a50e",
            "f501dc470f4ec7d5dcb16851f758a03741df6d74ba907192b88cccd935ad2440",
            "4cda780d55bb139b64c00fd9c580f6f42a3106d46c5db05190902de66528be22",
        ),
        _ => return None,
    };
    Some(Source {
        repository: repository.into(),
        revision: revision.into(),
        model_file: "onnx/onnx_fp16/model_fp16.onnx".into(),
        model_sha256: model_sha256.into(),
        tokenizer_file: "onnx/onnx_fp16/tokenizer.json".into(),
        tokenizer_sha256: tokenizer_sha256.into(),
        injection_index: 1,
    })
}

/// The settings file.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// A built-in model, or any name when `source` is given.
    #[serde(default = "default_model")]
    pub model: String,
    /// The files of a model that is not built in.
    #[serde(default)]
    pub source: Option<Source>,
    /// Where models are kept; by default `$XDG_CACHE_HOME/amaru-guard` or
    /// `~/.cache/amaru-guard`.
    #[serde(default)]
    pub cache_dir: Option<PathBuf>,
    /// Reject a text whose injection probability is above this, in percent.
    #[serde(default = "default_threshold")]
    pub threshold_percent: f64,
    /// Tokens per model input, including the special tokens.
    #[serde(default = "default_window")]
    pub window_tokens: usize,
    /// Tokens that consecutive windows share.
    #[serde(default = "default_overlap")]
    pub overlap_tokens: usize,
    /// CPU threads for the model; ONNX Runtime chooses when absent.
    #[serde(default)]
    pub threads: Option<usize>,
    /// Where the files are downloaded from.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// The environment variable with a Hugging Face token, when one is
    /// needed.
    #[serde(default = "default_token_env")]
    pub token_env: String,
}

fn default_model() -> String {
    DEFAULT_MODEL.into()
}
/// The decision threshold of the model cards' evaluation.
const fn default_threshold() -> f64 {
    50.0
}
/// The window the Wolf Defender models were trained with.
const fn default_window() -> usize {
    2048
}
const fn default_overlap() -> usize {
    64
}
fn default_endpoint() -> String {
    "https://huggingface.co".into()
}
fn default_token_env() -> String {
    "HF_TOKEN".into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: default_model(),
            source: None,
            cache_dir: None,
            threshold_percent: default_threshold(),
            window_tokens: default_window(),
            overlap_tokens: default_overlap(),
            threads: None,
            endpoint: default_endpoint(),
            token_env: default_token_env(),
        }
    }
}

impl Config {
    /// Read and check a settings file; the defaults without one.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config: Self = match path {
            None => Self::default(),
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("cannot read {}", path.display()))?;
                serde_yaml::from_str(&text)
                    .with_context(|| format!("invalid {}", path.display()))?
            }
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if !(0.0..=100.0).contains(&self.threshold_percent) {
            bail!("threshold_percent must be between 0 and 100");
        }
        if self.window_tokens < 8 {
            bail!("window_tokens must be at least 8");
        }
        if self.overlap_tokens * 2 >= self.window_tokens {
            bail!("overlap_tokens must be less than half of window_tokens");
        }
        let source = self.source()?;
        for hash in [&source.model_sha256, &source.tokenizer_sha256] {
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                bail!("{hash:?} is not a SHA-256 in hex");
            }
        }
        Ok(())
    }

    /// The files of the configured model.
    pub fn source(&self) -> Result<Source> {
        match (&self.source, preset(&self.model)) {
            (Some(source), _) => Ok(source.clone()),
            (None, Some(source)) => Ok(source),
            (None, None) => bail!(
                "model {:?} is not built in: give its `source`, or use \
                 wolf-defender-prompt-injection-small or wolf-defender-prompt-injection",
                self.model
            ),
        }
    }

    /// The folder of the configured model's files.
    pub fn model_dir(&self) -> Result<PathBuf> {
        let source = self.source()?;
        let cache = match &self.cache_dir {
            Some(dir) => dir.clone(),
            None => default_cache()?,
        };
        Ok(cache.join(&source.repository).join(&source.revision))
    }
}

fn default_cache() -> Result<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    if let Some(cache) = var("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(cache).join("amaru-guard"));
    }
    let home = var("HOME").context("set cache_dir: HOME is not set")?;
    Ok(PathBuf::from(home).join(".cache/amaru-guard"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config> {
        let config: Config = serde_yaml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    #[test]
    fn defaults_to_the_small_model() {
        let config = Config::load(None).unwrap();
        let source = config.source().unwrap();
        assert_eq!(
            source.repository,
            "patronus-studio/wolf-defender-prompt-injection-small"
        );
        assert!(source.model_file.ends_with("model_fp16.onnx"));
        assert!((config.threshold_percent - 50.0).abs() < f64::EPSILON);
        assert_eq!((config.window_tokens, config.overlap_tokens), (2048, 64));
    }

    #[test]
    fn keeps_models_under_their_revision() {
        let config = parse("model: wolf-defender-prompt-injection\ncache_dir: /c\n").unwrap();
        assert_eq!(
            config.model_dir().unwrap(),
            Path::new(
                "/c/patronus-studio/wolf-defender-prompt-injection/af0b16a1d40ca246595e48246ccf9f906a52a50e"
            )
        );
    }

    #[test]
    fn rejects_bad_settings() {
        assert!(parse("treshold_percent: 80\n").is_err());
        assert!(parse("threshold_percent: 180\n").is_err());
        assert!(parse("window_tokens: 100\noverlap_tokens: 50\n").is_err());
        assert!(parse("model: other\n").is_err());
        let source = "source: {repository: a/b, revision: r, model_file: m, \
                      model_sha256: abc, tokenizer_file: t, tokenizer_sha256: abc}\n";
        assert!(parse(&format!("model: other\n{source}")).is_err());
        let good = source.replace("abc", &"a".repeat(64));
        let config = parse(&format!("model: other\n{good}")).unwrap();
        assert_eq!(config.source().unwrap().injection_index, 1);
    }
}
