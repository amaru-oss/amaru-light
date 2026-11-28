//! The guard end to end with a stand-in model in ONNX Runtime.
//!
//! The stand-in tokenizer makes each word one token: `attack` is 9, any
//! other word 4, and it wraps a text in `[CLS]` (1) and `[SEP]` (2). The
//! stand-in graph gives the logits `[6, max(input_ids)]`, so a window with
//! `attack` leans to injection (p = 0.95) and any other to benign (0.12).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use amaru_guard::{
    check,
    classify::Classifier,
    config::Config,
    download::{Files, files},
};
use serde_json::json;
use std::{io::Write, path::Path, process::Stdio};

/// Protocol Buffers encoding, enough for a small ONNX model (as in
/// midnight-saci's `saci-julia` tests).
mod proto {
    #[allow(clippy::cast_possible_truncation)] // Varints keep 7 bits per byte on purpose.
    pub fn varint(mut value: u64, out: &mut Vec<u8>) {
        while value >= 0x80 {
            out.push((value as u8) | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }

    pub fn number(field: u64, value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        varint(field << 3, &mut out);
        varint(value, &mut out);
        out
    }

    pub fn bytes(field: u64, value: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        varint((field << 3) | 2, &mut out);
        varint(value.len() as u64, &mut out);
        out.extend_from_slice(value);
        out
    }

    pub fn text(field: u64, value: &str) -> Vec<u8> {
        bytes(field, value.as_bytes())
    }
}

const FLOAT: u64 = 1;
const INT64: u64 = 7;
const ATTRIBUTE_INT: u64 = 2;

fn value_info(name: &str, elem_type: u64, dims: &[&str]) -> Vec<u8> {
    let shape: Vec<u8> = dims
        .iter()
        .flat_map(|dim| proto::bytes(1, &proto::text(2, dim)))
        .collect();
    let tensor = [proto::number(1, elem_type), proto::bytes(2, &shape)].concat();
    [
        proto::text(1, name),
        proto::bytes(2, &proto::bytes(1, &tensor)),
    ]
    .concat()
}

fn int_attribute(name: &str, value: u64) -> Vec<u8> {
    [
        proto::text(1, name),
        proto::number(3, value),
        proto::number(20, ATTRIBUTE_INT),
    ]
    .concat()
}

fn node(op: &str, inputs: &[&str], output: &str, attributes: &[Vec<u8>]) -> Vec<u8> {
    let mut node: Vec<u8> = inputs.iter().flat_map(|i| proto::text(1, i)).collect();
    node.extend(proto::text(2, output));
    node.extend(proto::text(3, output));
    node.extend(proto::text(4, op));
    for attribute in attributes {
        node.extend(proto::bytes(5, attribute));
    }
    node
}

/// `logits = Concat([[6.0]], Cast(ReduceMax(input_ids), FLOAT), axis = 1)`.
#[allow(clippy::too_many_lines)] // One protobuf message after another.
fn stand_in_model() -> Vec<u8> {
    let six = [
        proto::number(1, 1),
        proto::number(1, 1),
        proto::number(2, FLOAT),
        proto::text(8, "six"),
        proto::bytes(9, &6.0_f32.to_le_bytes()),
    ]
    .concat();
    let nodes = [
        node("ReduceMax", &["input_ids"], "max", &[]),
        node("Cast", &["max"], "max_float", &[int_attribute("to", FLOAT)]),
        node(
            "Concat",
            &["six", "max_float"],
            "logits",
            &[int_attribute("axis", 1)],
        ),
    ];
    let mut graph: Vec<u8> = nodes.iter().flat_map(|n| proto::bytes(1, n)).collect();
    graph.extend(proto::text(2, "stand-in"));
    graph.extend(proto::bytes(5, &six));
    for name in ["input_ids", "attention_mask"] {
        graph.extend(proto::bytes(
            11,
            &value_info(name, INT64, &["batch", "tokens"]),
        ));
    }
    graph.extend(proto::bytes(
        12,
        &value_info("logits", FLOAT, &["batch", "classes"]),
    ));
    [
        proto::number(1, 8), // IR version
        proto::bytes(7, &graph),
        proto::bytes(8, &[proto::text(1, ""), proto::number(2, 18)].concat()),
    ]
    .concat()
}

fn stand_in_tokenizer() -> String {
    let special = |name: &str| json!({"SpecialToken": {"id": name, "type_id": 0}});
    let template =
        json!([special("[CLS]"), {"Sequence": {"id": "A", "type_id": 0}}, special("[SEP]")]);
    json!({
        "version": "1.0", "truncation": null, "padding": null, "added_tokens": [],
        "normalizer": null, "pre_tokenizer": {"type": "Whitespace"},
        "post_processor": {
            "type": "TemplateProcessing", "single": template, "pair": template,
            "special_tokens": {
                "[CLS]": {"id": "[CLS]", "ids": [1], "tokens": ["[CLS]"]},
                "[SEP]": {"id": "[SEP]", "ids": [2], "tokens": ["[SEP]"]}
            }
        },
        "decoder": null,
        "model": {"type": "WordLevel", "unk_token": "[UNK]",
                  "vocab": {"[CLS]": 1, "[SEP]": 2, "[UNK]": 4, "attack": 9}}
    })
    .to_string()
}

/// A settings file whose model is the stand-in, already in the cache.
fn setup(dir: &Path, window: usize) -> (Config, Files) {
    let hash = "0".repeat(64);
    let text = format!(
        "model: stand-in\ncache_dir: {}\nwindow_tokens: {window}\noverlap_tokens: 2\nthreads: 1\n\
         endpoint: http://127.0.0.1:9\n\
         source: {{repository: test/stand-in, revision: r1, model_file: model.onnx, \
         model_sha256: {hash}, tokenizer_file: tokenizer.json, tokenizer_sha256: {hash}}}\n",
        dir.display()
    );
    let path = dir.join("guard.yaml");
    std::fs::write(&path, text).unwrap();
    let config = Config::load(Some(&path)).unwrap();
    let local = files(&config.model_dir().unwrap(), &config.source().unwrap());
    std::fs::create_dir_all(local.model.parent().unwrap()).unwrap();
    std::fs::write(&local.model, stand_in_model()).unwrap();
    std::fs::write(&local.tokenizer, stand_in_tokenizer()).unwrap();
    (config, local)
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

#[test]
fn scores_one_window_with_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let (config, local) = setup(dir.path(), 2048);
    let classifier = Classifier::load(&local, &config).unwrap();
    let benign = classifier.score("please add a button").unwrap();
    assert_eq!(benign.windows, 1);
    assert!(
        (benign.injection - sigmoid(4.0 - 6.0)).abs() < 1e-6,
        "{benign:?}"
    );
    let attack = classifier.score("please attack").unwrap();
    assert!(
        (attack.injection - sigmoid(9.0 - 6.0)).abs() < 1e-6,
        "{attack:?}"
    );
}

#[test]
fn finds_an_injection_at_the_end_of_a_long_text() {
    let dir = tempfile::tempdir().unwrap();
    // Six words per window, two shared: 21 words make 5 windows.
    let (config, _) = setup(dir.path(), 8);
    let text = format!("{} attack", ["word"; 20].join(" "));
    let decision = check(&config, &text).unwrap();
    assert_eq!(decision.windows, 5);
    assert!(decision.rejected(), "{decision:?}");
    let expected = sigmoid(9.0 + 0.1 * ((1.0 + 4.0 * (-50.0_f64).exp()).ln() - 5.0_f64.ln()) - 6.0);
    assert!((decision.score - expected).abs() < 1e-6, "{decision:?}");
    let benign = check(&config, &["word"; 21].join(" ")).unwrap();
    assert!(!benign.rejected(), "{benign:?}");
}

fn run(config: &Path, text: &str) -> (Option<i32>, String) {
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_amaru-guard"))
        .args(["check", "-c", &config.display().to_string(), "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn the_command_exits_with_the_decision() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path(), 2048);
    let config = dir.path().join("guard.yaml");
    let (code, out) = run(&config, "add a button");
    assert_eq!(code, Some(0), "{out}");
    let answer: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(answer["label"], "benign");
    let (code, out) = run(&config, "attack the bot");
    assert_eq!(code, Some(1), "{out}");
    assert!(
        !out.contains("bot"),
        "the answer never repeats the text: {out}"
    );
    std::fs::write(&config, "model: unknown\n").unwrap();
    assert_eq!(run(&config, "add a button").0, Some(2));
}
