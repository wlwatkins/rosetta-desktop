use anyhow::{anyhow, Context, Result};
use std::path::Path;
use tokenizers::Tokenizer;

/// SentencePiece's word-boundary marker.
const SPACE_MARKER: char = '\u{2581}';

pub struct MarianTokenizer {
    inner: Tokenizer,
    /// id -> piece, used for detokenizing the English output.
    pieces: Vec<String>,
    specials: Vec<i64>,
}

impl MarianTokenizer {
    pub fn load(dir: &Path, specials: Vec<i64>) -> Result<Self> {
        let tok_path = dir.join("tokenizer_source.json");
        let inner = Tokenizer::from_file(&tok_path)
            .map_err(|e| anyhow!("loading {}: {e}", tok_path.display()))?;

        let vocab_path = dir.join("vocab_target.json");
        let raw = std::fs::read_to_string(&vocab_path)
            .with_context(|| format!("reading {}", vocab_path.display()))?;
        let pieces: Vec<String> =
            serde_json::from_str(&raw).with_context(|| format!("parsing {}", vocab_path.display()))?;

        Ok(Self { inner, pieces, specials })
    }

    /// Hebrew text -> token ids (the tokenizer's post-processor appends </s>).
    pub fn encode(&self, text: &str) -> Result<Vec<i64>> {
        let enc = self
            .inner
            .encode(text, true)
            .map_err(|e| anyhow!("tokenizing {text:?}: {e}"))?;
        Ok(enc.get_ids().iter().map(|&i| i as i64).collect())
    }

    /// Token ids -> English text.
    pub fn decode(&self, ids: &[i64]) -> String {
        let mut out = String::new();
        for &id in ids {
            if self.specials.contains(&id) {
                continue;
            }
            let Some(piece) = self.pieces.get(id as usize) else {
                continue;
            };
            for ch in piece.chars() {
                if ch == SPACE_MARKER {
                    out.push(' ');
                } else {
                    out.push(ch);
                }
            }
        }
        out.trim().to_string()
    }
}
