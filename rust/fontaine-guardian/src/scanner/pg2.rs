//! PromptGuard 2 (86M) through candle. The weights are never bundled: the caller points this at
//! a directory holding `model.safetensors`, `tokenizer.json` and `config.json`.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::debertav2::{Config, DebertaV2SeqClassificationModel};
use futures::future::BoxFuture;
use tokenizers::Tokenizer;

use super::{Scan, Scanner, failed};

/// A malicious-class probability at or above this flags the result.
pub const FLAG_THRESHOLD: f64 = 0.9;
/// Tokens per scored window, the special tokens included.
pub const WINDOW_TOKENS: usize = 512;
pub const STRIDE_TOKENS: usize = 256;

/// Label names that mark the class to flag, lower-cased.
const MALICIOUS_LABELS: [&str; 4] = ["malicious", "injection", "jailbreak", "unsafe"];

struct Loaded {
    model: DebertaV2SeqClassificationModel,
    tokenizer: Tokenizer,
    malicious: usize,
}

pub struct Pg2Scanner {
    loaded: Arc<Loaded>,
    timeout: Duration,
}

/// Index of the class to flag. The model card's labels are `LABEL_0` (benign) and `LABEL_1`
/// (malicious), so an unnamed label map falls back to the last class.
fn malicious_class(id2label: &HashMap<u32, String>) -> Result<usize, String> {
    let named = id2label.iter().find(|(_, name)| {
        let name = name.to_ascii_lowercase();
        MALICIOUS_LABELS.iter().any(|label| name.contains(label))
    });
    let id = named
        .map(|(id, _)| *id)
        .or_else(|| id2label.keys().max().copied())
        .ok_or("config.json has an empty id2label")?;
    Ok(id as usize)
}

/// The ranges of `len` body tokens each window covers: one when everything fits, else
/// `size`-wide windows every `stride`, the last one reaching the end.
fn window_ranges(len: usize, size: usize, stride: usize) -> Vec<Range<usize>> {
    if len <= size {
        return std::iter::once(0..len).collect();
    }
    let mut ranges = Vec::new();
    let mut start = 0;
    loop {
        let end = (start + size).min(len);
        ranges.push(start..end);
        if end == len {
            return ranges;
        }
        start += stride;
    }
}

impl Loaded {
    fn open(dir: &Path) -> Result<Loaded, String> {
        let read = |name: &str| {
            std::fs::read(dir.join(name)).map_err(|e| format!("cannot read {name}: {e}"))
        };
        let config_bytes = read("config.json")?;
        let config: Config = serde_json::from_slice(&config_bytes)
            .map_err(|e| format!("config.json is not a DeBERTa configuration: {e}"))?;
        // The published PromptGuard 2 config has no label map; transformers defaults it the same way.
        let id2label = config.id2label.clone().unwrap_or_else(|| {
            HashMap::from([(0, "LABEL_0".to_string()), (1, "LABEL_1".to_string())])
        });
        let malicious = malicious_class(&id2label)?;

        let mut tokenizer = Tokenizer::from_bytes(read("tokenizer.json")?)
            .map_err(|e| format!("tokenizer.json is unusable: {e}"))?;
        // The whole result is scored in windows, so nothing may cut it short.
        tokenizer
            .with_truncation(None)
            .map_err(|e| format!("tokenizer.json is unusable: {e}"))?;
        tokenizer.with_padding(None);

        // Read into memory rather than mapped: a mapping would need `unsafe`, and the file is
        // dropped as soon as the tensors are built.
        let weights = VarBuilder::from_buffered_safetensors(
            read("model.safetensors")?,
            DType::F32,
            &Device::Cpu,
        )
        .map_err(|e| format!("model.safetensors is unusable: {e}"))?;
        let model =
            DebertaV2SeqClassificationModel::load(weights.pp("deberta"), &config, Some(id2label))
                .map_err(|e| format!("the model cannot be built from its files: {e}"))?;
        Ok(Loaded {
            model,
            tokenizer,
            malicious,
        })
    }

    fn window_score(&self, ids: &[u32]) -> candle_core::Result<f64> {
        let device = &self.model.device;
        let input = Tensor::new(ids, device)?.unsqueeze(0)?;
        let mask = input.ones_like()?;
        let kinds = input.zeros_like()?;
        let logits = self.model.forward(&input, Some(kinds), Some(mask))?;
        let probabilities = candle_nn::ops::softmax(&logits, 1)?;
        let score: f32 = probabilities.get(0)?.get(self.malicious)?.to_scalar()?;
        Ok(f64::from(score))
    }

    /// The highest malicious-class score over the windows of `content`. Stops early once a
    /// window is flagged or `cancel` is set.
    fn score(&self, content: &str, cancel: &AtomicBool) -> Result<f64, String> {
        let encoding = self
            .tokenizer
            .encode(content, true)
            .map_err(|e| format!("cannot tokenize the result: {e}"))?;
        let ids = encoding.get_ids();
        let mut highest = 0.0_f64;
        // The first and last ids are the special tokens the tokenizer wraps the text in; every
        // window carries them, so a window is `WINDOW_TOKENS` tokens in all.
        let windows: Vec<Vec<u32>> = if ids.len() <= WINDOW_TOKENS {
            vec![ids.to_vec()]
        } else {
            let (first, last) = (ids[0], ids[ids.len() - 1]);
            let body = &ids[1..ids.len() - 1];
            window_ranges(body.len(), WINDOW_TOKENS - 2, STRIDE_TOKENS)
                .into_iter()
                .map(|range| {
                    let mut window = Vec::with_capacity(range.len() + 2);
                    window.push(first);
                    window.extend_from_slice(&body[range]);
                    window.push(last);
                    window
                })
                .collect()
        };
        for window in windows {
            if cancel.load(Ordering::Relaxed) {
                return Err("the scan was abandoned after its time ran out".into());
            }
            let score = self
                .window_score(&window)
                .map_err(|e| format!("inference failed: {e}"))?;
            highest = highest.max(finite_score(score)?);
            if highest >= FLAG_THRESHOLD {
                break;
            }
        }
        Ok(highest)
    }
}

/// `f64::max` ignores NaN and NaN never reaches the threshold, so a non-finite score would
/// read as clean; it has to fail the scan instead.
fn finite_score(score: f64) -> Result<f64, String> {
    if score.is_finite() {
        Ok(score)
    } else {
        Err(format!("inference produced a non-finite score ({score})"))
    }
}

impl Pg2Scanner {
    /// Loads the model from `dir`, off the async workers and within `timeout`.
    pub async fn load(dir: &Path, timeout: Duration) -> Result<Pg2Scanner, String> {
        let dir = dir.to_path_buf();
        let task = tokio::task::spawn_blocking(move || Loaded::open(&dir));
        match tokio::time::timeout(timeout, task).await {
            Err(_) => Err(format!(
                "loading the model took longer than {} seconds",
                timeout.as_secs()
            )),
            Ok(Err(e)) => Err(format!("loading the model failed: {e}")),
            Ok(Ok(loaded)) => Ok(Pg2Scanner {
                loaded: Arc::new(loaded?),
                timeout,
            }),
        }
    }

    /// Tokenization and inference are synchronous work, so they run on the blocking pool
    /// and the timeout covers the whole scan.
    async fn run(&self, content: &str) -> Scan {
        let loaded = self.loaded.clone();
        let text = content.to_string();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let task = tokio::task::spawn_blocking(move || loaded.score(&text, &stop));
        match tokio::time::timeout(self.timeout, task).await {
            Err(_) => {
                cancel.store(true, Ordering::Relaxed);
                failed(format!(
                    "the scan did not finish within {} seconds",
                    self.timeout.as_secs()
                ))
            }
            Ok(Err(e)) => failed(format!("the scan failed: {e}")),
            Ok(Ok(Err(message))) => failed(message),
            Ok(Ok(Ok(score))) if score >= FLAG_THRESHOLD => Scan::Flagged {
                reason: format!("PromptGuard 2 scored this result {score:.2} for injection"),
            },
            Ok(Ok(Ok(_))) => Scan::Clean,
        }
    }
}

impl Scanner for Pg2Scanner {
    fn scan<'a>(&'a self, _tool: &'a str, content: &'a str) -> BoxFuture<'a, Scan> {
        Box::pin(self.run(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_finite_score_fails_the_scan() {
        assert_eq!(finite_score(0.25), Ok(0.25));
        assert!(finite_score(f64::NAN).is_err());
        assert!(finite_score(f64::INFINITY).is_err());
        assert!(finite_score(f64::NEG_INFINITY).is_err());
    }

    #[test]
    fn short_content_is_one_window() {
        assert_eq!(window_ranges(0, 510, 256), vec![0..0]);
        assert_eq!(window_ranges(510, 510, 256), vec![0..510]);
    }

    #[test]
    fn long_content_slides_by_the_stride_and_the_last_window_reaches_the_end() {
        assert_eq!(window_ranges(511, 510, 256), [0..510, 256..511]);
        assert_eq!(window_ranges(766, 510, 256), [0..510, 256..766]);
        assert_eq!(window_ranges(1000, 510, 256), [0..510, 256..766, 512..1000]);
    }

    #[test]
    fn the_malicious_class_is_found_by_name_or_is_the_last() {
        let named = HashMap::from([(0, "BENIGN".to_string()), (1, "MALICIOUS".to_string())]);
        assert_eq!(malicious_class(&named), Ok(1));
        let swapped = HashMap::from([(0, "malicious".to_string()), (1, "benign".to_string())]);
        assert_eq!(malicious_class(&swapped), Ok(0));
        let plain = HashMap::from([(0, "LABEL_0".to_string()), (1, "LABEL_1".to_string())]);
        assert_eq!(malicious_class(&plain), Ok(1));
        assert!(malicious_class(&HashMap::new()).is_err());
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;
    use tokenizers::models::wordlevel::WordLevel;
    use tokenizers::pre_tokenizers::whitespace::Whitespace;

    const SMALL_CONFIG: &str = r#"{
        "vocab_size": 8, "hidden_size": 16, "num_hidden_layers": 1, "num_attention_heads": 2,
        "intermediate_size": 32, "hidden_act": "gelu", "hidden_dropout_prob": 0.0,
        "attention_probs_dropout_prob": 0.0, "max_position_embeddings": 512,
        "type_vocab_size": 0, "initializer_range": 0.02, "layer_norm_eps": 1e-7,
        "relative_attention": true, "max_relative_positions": -1, "pad_token_id": 0,
        "position_biased_input": false, "pos_att_type": ["p2c", "c2p"], "position_buckets": 16,
        "share_att_key": true, "norm_rel_ebd": "layer_norm", "pooler_hidden_size": 16,
        "pooler_dropout": 0.0, "pooler_hidden_act": "gelu",
        "id2label": {"0": "LABEL_0", "1": "LABEL_1"}
    }"#;

    /// A tiny model with all-zero weights: it cannot classify, but it runs the same tensor
    /// path as the real one, so a shape, dtype or windowing mistake shows up as an error.
    fn tiny() -> Loaded {
        let config: Config = serde_json::from_str(SMALL_CONFIG).unwrap();
        let weights = VarBuilder::zeros(DType::F32, &Device::Cpu);
        let model =
            DebertaV2SeqClassificationModel::load(weights, &config, config.id2label.clone())
                .unwrap();
        let vocab = [("[UNK]", 0), ("a", 1), ("b", 2)]
            .into_iter()
            .map(|(word, id)| (word.to_string(), id))
            .collect();
        let words = WordLevel::builder()
            .vocab(vocab)
            .unk_token("[UNK]".into())
            .build()
            .unwrap();
        let mut tokenizer = Tokenizer::new(words);
        tokenizer.with_pre_tokenizer(Some(Whitespace {}));
        Loaded {
            model,
            tokenizer,
            malicious: 1,
        }
    }

    #[test]
    fn a_checkpoint_in_the_published_layout_loads() {
        let config_json = SMALL_CONFIG.replace(
            ",\n        \"id2label\": {\"0\": \"LABEL_0\", \"1\": \"LABEL_1\"}",
            "",
        );
        assert!(!config_json.contains("id2label"));
        let config: Config = serde_json::from_str(&config_json).unwrap();
        let varmap = candle_nn::VarMap::new();
        let vb = VarBuilder::from_varmap(&varmap, DType::F32, &Device::Cpu);
        let labels = HashMap::from([(0, "LABEL_0".to_string()), (1, "LABEL_1".to_string())]);
        DebertaV2SeqClassificationModel::load(vb.pp("deberta"), &config, Some(labels)).unwrap();

        let dir = tempfile::tempdir().unwrap();
        varmap.save(dir.path().join("model.safetensors")).unwrap();
        std::fs::write(dir.path().join("config.json"), config_json).unwrap();
        tiny()
            .tokenizer
            .save(dir.path().join("tokenizer.json"), false)
            .unwrap();

        let loaded = Loaded::open(dir.path()).unwrap();
        assert_eq!(loaded.malicious, 1);
    }

    #[test]
    fn short_and_long_content_both_score_without_error() {
        let loaded = tiny();
        let cancel = AtomicBool::new(false);
        for tokens in [1_usize, 100, 510, 513, 1300] {
            let text = vec!["a b"; tokens.div_ceil(2)].join(" ");
            let score = loaded.score(&text, &cancel).unwrap();
            assert!((score - 0.5).abs() < 1e-6, "{tokens}: {score}");
        }
    }

    #[test]
    fn a_cancelled_scan_stops() {
        let loaded = tiny();
        let cancel = AtomicBool::new(true);
        assert!(loaded.score("a b a b", &cancel).is_err());
    }
}
