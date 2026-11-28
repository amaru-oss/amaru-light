//! Runs a Wolf Defender style classifier: the text is tokenized once, cut
//! into overlapping windows that each get the tokenizer's special tokens,
//! and the window logits are pooled the way the model's reference code does.

use crate::{config::Config, download::Files};
use anyhow::{Context, Result, anyhow, bail};
use ort::{
    session::{Session, builder::GraphOptimizationLevel},
    value::Tensor,
};
use tokenizers::Tokenizer;

/// The decision for one text.
#[derive(Clone, Debug, PartialEq)]
pub struct Score {
    /// Probability of the injection class, from 0 to 1.
    pub injection: f64,
    /// How many windows the text took.
    pub windows: usize,
}

/// Cut `tokens` into windows of at most `size` tokens, each starting
/// `overlap` tokens before the previous one ended. An empty text is one
/// empty window.
pub fn windows(tokens: &[u32], size: usize, overlap: usize) -> Vec<&[u32]> {
    let mut out = Vec::new();
    let mut start = 0;
    loop {
        let end = (start + size).min(tokens.len());
        out.push(&tokens[start..end]);
        if end == tokens.len() {
            return out;
        }
        start = end - overlap;
    }
}

/// Pool the logits of each window into class probabilities: a smooth
/// maximum per class (temperature 1 for one window, 0.1 for several, minus
/// the log of the count), then a softmax. This is the reference pooling of
/// the Wolf Defender models (`POOLING.json`, `inference.py`).
pub fn pool(logits: &[Vec<f32>]) -> Vec<f64> {
    let Some(first) = logits.first() else {
        return Vec::new();
    };
    let count = logits.len();
    let temperature = if count == 1 { 1.0 } else { 0.1 };
    #[allow(clippy::cast_precision_loss)] // A window count is far below 2^52.
    let log_count = (count as f64).ln();
    let pooled: Vec<f64> = (0..first.len())
        .map(|class| {
            let values: Vec<f64> = logits.iter().map(|w| f64::from(w[class])).collect();
            let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let sum: f64 = values.iter().map(|v| ((v - max) / temperature).exp()).sum();
            max + temperature * (sum.ln() - log_count)
        })
        .collect();
    let top = pooled.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exps: Vec<f64> = pooled.iter().map(|p| (p - top).exp()).collect();
    let total: f64 = exps.iter().sum();
    exps.iter().map(|e| e / total).collect()
}

/// A loaded model and its tokenizer.
pub struct Classifier {
    tokenizer: Box<Tokenizer>,
    session: Session,
    /// The output that holds the logits.
    output: String,
    /// Special tokens before and after the text of each window.
    prefix: Vec<u32>,
    suffix: Vec<u32>,
    window: usize,
    overlap: usize,
    injection_index: usize,
}

impl std::fmt::Debug for Classifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Classifier")
            .field("prefix", &self.prefix)
            .field("suffix", &self.suffix)
            .field("window", &self.window)
            .finish_non_exhaustive()
    }
}

/// The special tokens the tokenizer puts around a text, found by encoding
/// one word with and without them.
fn specials(tokenizer: &Tokenizer) -> Result<(Vec<u32>, Vec<u32>)> {
    let encode = |special: bool| {
        tokenizer
            .encode("a", special)
            .map(|e| e.get_ids().to_vec())
            .map_err(|e| anyhow!("cannot tokenize: {e}"))
    };
    let (with, without) = (encode(true)?, encode(false)?);
    let at = (0..=with.len().saturating_sub(without.len()))
        .find(|&i| with[i..].starts_with(&without))
        .context("the tokenizer changes the text when it adds special tokens")?;
    Ok((with[..at].to_vec(), with[at + without.len()..].to_vec()))
}

/// The tokenizer, without the truncation and padding of its file: the
/// windows decide the length.
fn tokenizer(files: &Files) -> Result<Box<Tokenizer>> {
    let mut tokenizer = Box::new(
        Tokenizer::from_file(&files.tokenizer)
            .map_err(|e| anyhow!("cannot load {}: {e}", files.tokenizer.display()))?,
    );
    tokenizer
        .with_truncation(None)
        .map_err(|e| anyhow!("cannot turn truncation off: {e}"))?;
    tokenizer.with_padding(None);
    Ok(tokenizer)
}

fn session(files: &Files, config: &Config) -> Result<Session> {
    let mut builder =
        Session::builder()?.with_optimization_level(GraphOptimizationLevel::Level3)?;
    if let Some(threads) = config.threads {
        builder = builder.with_intra_threads(threads)?;
    }
    builder
        .commit_from_file(&files.model)
        .with_context(|| format!("cannot load {}", files.model.display()))
}

/// The values of a float tensor, in FP32 or FP16.
fn floats(value: &ort::value::DynValue) -> Result<Vec<f32>> {
    if let Ok((_, data)) = value.try_extract_raw_tensor::<f32>() {
        return Ok(data.to_vec());
    }
    let (_, data) = value.try_extract_raw_tensor::<half::f16>()?;
    Ok(data.iter().map(|v| v.to_f32()).collect())
}

impl Classifier {
    /// Load the model and the tokenizer.
    pub fn load(files: &Files, config: &Config) -> Result<Self> {
        let tokenizer = tokenizer(files)?;
        let (prefix, suffix) = specials(&tokenizer)?;
        let window = config
            .window_tokens
            .checked_sub(prefix.len() + suffix.len())
            .filter(|w| *w > config.overlap_tokens)
            .context("window_tokens leaves no room for the text")?;
        let session = session(files, config)?;
        let names: Vec<&str> = session.outputs.iter().map(|o| o.name.as_str()).collect();
        let output = names
            .iter()
            .find(|n| **n == "logits")
            .or_else(|| names.first())
            .context("the model has no output")?
            .to_string();
        Ok(Self {
            tokenizer,
            session,
            output,
            prefix,
            suffix,
            window,
            overlap: config.overlap_tokens,
            injection_index: config.source()?.injection_index,
        })
    }

    /// The injection probability of `text`.
    pub fn score(&self, text: &str) -> Result<Score> {
        let tokens = self
            .tokenizer
            .encode(text, false)
            .map_err(|e| anyhow!("cannot tokenize: {e}"))?;
        let parts = windows(tokens.get_ids(), self.window, self.overlap);
        let logits = parts
            .iter()
            .map(|part| self.logits(part))
            .collect::<Result<Vec<_>>>()?;
        let probabilities = pool(&logits);
        let injection = *probabilities
            .get(self.injection_index)
            .with_context(|| format!("the model has {} classes", probabilities.len()))?;
        Ok(Score {
            injection,
            windows: parts.len(),
        })
    }

    /// The raw logits of one window.
    fn logits(&self, part: &[u32]) -> Result<Vec<f32>> {
        let ids: Vec<i64> = self
            .prefix
            .iter()
            .chain(part)
            .chain(&self.suffix)
            .map(|&id| i64::from(id))
            .collect();
        let len = ids.len();
        let mut inputs = ort::inputs! {
            "input_ids" => Tensor::from_array(([1, len], ids))?,
            "attention_mask" => Tensor::from_array(([1, len], vec![1_i64; len]))?,
        }?;
        if self
            .session
            .inputs
            .iter()
            .any(|i| i.name == "token_type_ids")
        {
            let types = Tensor::from_array(([1, len], vec![0_i64; len]))?;
            inputs.push(("token_type_ids".into(), types.into()));
        }
        let outputs = self.session.run(inputs)?;
        let values = floats(&outputs[self.output.as_str()])?;
        if values.len() < 2 {
            bail!(
                "the model gave {} logits, expected one per class",
                values.len()
            );
        }
        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_overlap_and_cover_every_token() {
        let tokens: Vec<u32> = (0..10).collect();
        let parts = windows(&tokens, 4, 1);
        assert_eq!(parts, [&[0, 1, 2, 3][..], &[3, 4, 5, 6], &[6, 7, 8, 9]]);
        assert_eq!(windows(&tokens, 20, 2), [&tokens[..]]);
        let empty: &[u32] = &[];
        assert_eq!(windows(empty, 4, 1), [empty]);
    }

    #[test]
    fn one_window_is_a_plain_softmax() {
        let p = pool(&[vec![0.0, 0.0]]);
        assert!((p[1] - 0.5).abs() < 1e-9);
        let p = pool(&[vec![1.0, 3.0]]);
        let expected = 3.0_f64.exp() / (1.0_f64.exp() + 3.0_f64.exp());
        assert!((p[1] - expected).abs() < 1e-9);
    }

    #[test]
    fn one_injected_window_outweighs_benign_ones() {
        // Class 0: 2 + 0.1 * (ln 2 - ln 3); class 1: 4 + 0.1 * (0 - ln 3).
        let windows = [vec![2.0, -2.0], vec![2.0, -2.0], vec![-4.0, 4.0]];
        let p = pool(&windows);
        let benign = 2.0 + 0.1 * (2.0_f64.ln() - 3.0_f64.ln());
        let injected = 4.0 - 0.1 * 3.0_f64.ln();
        let expected = 1.0 / (1.0 + (benign - injected).exp());
        assert!((p[1] - expected).abs() < 1e-6, "{p:?}");
        assert!(p[1] > 0.85);
        assert!((p.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    }
}
