#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! `amaru-guard`: classifies untrusted text with a local prompt injection
//! model before Amaru Light gives it to an agent.

pub mod classify;
pub mod config;
pub mod download;

use anyhow::Result;
use serde::Serialize;

/// What `check` prints: the decision, never the text.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Decision {
    /// `injection` or `benign`.
    pub label: &'static str,
    /// Probability of injection, from 0 to 1.
    pub score: f64,
    pub windows: usize,
    pub model: String,
}

impl Decision {
    pub fn rejected(&self) -> bool {
        self.label == "injection"
    }
}

/// Classify `text` with the configured model, downloading it first when it
/// is not in the cache.
pub fn check(config: &config::Config, text: &str) -> Result<Decision> {
    let files = download::ensure(config)?;
    let classifier = classify::Classifier::load(&files, config)?;
    let score = classifier.score(text)?;
    Ok(decide(config, &score))
}

/// Turn a score into a decision: injection when the probability is above
/// the threshold.
pub fn decide(config: &config::Config, score: &classify::Score) -> Decision {
    let rejected = score.injection * 100.0 > config.threshold_percent;
    Decision {
        label: if rejected { "injection" } else { "benign" },
        score: score.injection,
        windows: score.windows,
        model: config.model.clone(),
    }
}
