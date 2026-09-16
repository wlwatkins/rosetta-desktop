use anyhow::{anyhow, bail, Context, Result};
use ort::session::{builder::GraphOptimizationLevel, Session, SessionInputValue};
use ort::value::Tensor;
use serde::Deserialize;
use std::borrow::Cow;
use std::path::Path;

use super::tokenizer::MarianTokenizer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Cpu,
    DirectMl,
    Cuda,
}

impl Backend {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "cpu" => Some(Self::Cpu),
            "dml" | "directml" => Some(Self::DirectMl),
            "cuda" => Some(Self::Cuda),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub backend: Backend,
    pub threads: usize,
    /// 1 = greedy. Marian's own generation_config specifies 4.
    pub beams: usize,
    pub length_penalty: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self { backend: Backend::Cpu, threads: 4, beams: 4, length_penalty: 1.0 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // mirrors the exported JSON; not every field is read back
pub struct ModelMeta {
    pub d_model: usize,
    pub decoder_layers: usize,
    pub decoder_attention_heads: usize,
    pub head_dim: usize,
    pub vocab_size: usize,
    pub decoder_start_token_id: i64,
    pub eos_token_id: i64,
    pub pad_token_id: i64,
    pub unk_token_id: Option<i64>,
    pub max_length: usize,
}

/// optimum names the KV-cache tensors predictably, but the exact set varies with
/// the exporter version, so we discover the real names from the graph rather than
/// hardcoding them.
struct DecoderLayout {
    /// per layer: (self key, self value, cross key, cross value)
    past_in: Vec<(String, String, String, String)>,
    present_out: Vec<(String, String, String, String)>,
    wants_encoder_mask: bool,
    wants_cache_branch: bool,
}

impl DecoderLayout {
    fn discover(decoder: &Session, layers: usize) -> Result<Self> {
        let ins: Vec<String> = decoder.inputs().iter().map(|o| o.name().to_string()).collect();
        let outs: Vec<String> = decoder.outputs().iter().map(|o| o.name().to_string()).collect();

        let pick = |set: &[String], pat: &str| -> Result<String> {
            set.iter()
                .find(|n| n.as_str() == pat)
                .cloned()
                .ok_or_else(|| anyhow!("decoder graph has no `{pat}`; available: {set:?}"))
        };

        let mut past_in = Vec::with_capacity(layers);
        let mut present_out = Vec::with_capacity(layers);
        for l in 0..layers {
            past_in.push((
                pick(&ins, &format!("past_key_values.{l}.decoder.key"))?,
                pick(&ins, &format!("past_key_values.{l}.decoder.value"))?,
                pick(&ins, &format!("past_key_values.{l}.encoder.key"))?,
                pick(&ins, &format!("past_key_values.{l}.encoder.value"))?,
            ));
            present_out.push((
                pick(&outs, &format!("present.{l}.decoder.key"))?,
                pick(&outs, &format!("present.{l}.decoder.value"))?,
                pick(&outs, &format!("present.{l}.encoder.key"))?,
                pick(&outs, &format!("present.{l}.encoder.value"))?,
            ));
        }

        Ok(Self {
            past_in,
            present_out,
            wants_encoder_mask: ins.iter().any(|n| n == "encoder_attention_mask"),
            wants_cache_branch: ins.iter().any(|n| n == "use_cache_branch"),
        })
    }
}

/// A finished candidate translation.
struct Hypothesis {
    tokens: Vec<i64>,
    score: f32,
}

pub struct Marian {
    encoder: Session,
    decoder: Session,
    tok: MarianTokenizer,
    meta: ModelMeta,
    layout: DecoderLayout,
    cfg: Config,
    active_backend: Backend,
}

impl Marian {
    pub fn load(dir: &Path, cfg: Config) -> Result<Self> {
        let meta: ModelMeta = serde_json::from_str(
            &std::fs::read_to_string(dir.join("model_meta.json"))
                .context("reading model_meta.json")?,
        )
        .context("parsing model_meta.json")?;

        let (encoder, active_backend) = build_session(&dir.join("encoder_model.onnx"), &cfg)?;
        let (decoder, _) = build_session(&dir.join("decoder_model_merged.onnx"), &cfg)?;
        let layout = DecoderLayout::discover(&decoder, meta.decoder_layers)?;

        let mut specials = vec![meta.eos_token_id, meta.pad_token_id];
        if let Some(unk) = meta.unk_token_id {
            specials.push(unk);
        }
        let tok = MarianTokenizer::load(dir, specials)?;

        Ok(Self { encoder, decoder, tok, meta, layout, cfg, active_backend })
    }

    pub fn active_backend(&self) -> Backend {
        self.active_backend
    }

    pub fn translate(&mut self, text: &str) -> Result<String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(String::new());
        }

        let ids = self.tok.encode(text)?;
        if ids.is_empty() {
            return Ok(String::new());
        }
        let src_len = ids.len();
        if src_len > self.meta.max_length {
            bail!("input of {src_len} tokens exceeds max_length {}", self.meta.max_length);
        }

        let (hs_dims, hidden) = self.encode(&ids)?;
        let best = self.beam_search(&hs_dims, &hidden, src_len)?;
        Ok(self.tok.decode(&best))
    }

    fn encode(&mut self, ids: &[i64]) -> Result<(Vec<i64>, Vec<f32>)> {
        let n = ids.len() as i64;
        let out = self
            .encoder
            .run(ort::inputs! {
                "input_ids" => tensor_i64(&[1, n], ids.to_vec())?,
                "attention_mask" => tensor_i64(&[1, n], vec![1i64; ids.len()])?,
            })
            .map_err(|e| anyhow!("encoder: {e}"))?;

        let (shape, data) = out["last_hidden_state"]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow!("encoder output: {e}"))?;
        Ok((shape.iter().copied().collect(), data.to_vec()))
    }

    /// Batched beam search: the beams travel through the decoder as the batch
    /// dimension, and the self-attention cache is reordered each step to follow
    /// whichever beams survived.
    fn beam_search(&mut self, hs_dims: &[i64], hidden: &[f32], src_len: usize) -> Result<Vec<i64>> {
        let beams = self.cfg.beams.max(1);
        let b = beams as i64;
        let heads = self.meta.decoder_attention_heads as i64;
        let head_dim = self.meta.head_dim as i64;
        let layers = self.meta.decoder_layers;
        let d_model = hs_dims[2];

        // Every beam starts from the same encoder output.
        let tiled_hidden: Vec<f32> = hidden.repeat(beams);
        let tiled_dims = vec![b, src_len as i64, d_model];
        let mask = vec![1i64; src_len * beams];

        let mut cross_kv: Vec<(Vec<f32>, Vec<f32>)> = Vec::with_capacity(layers);
        let mut self_kv: Vec<(Vec<f32>, Vec<f32>)> =
            (0..layers).map(|_| (Vec::new(), Vec::new())).collect();
        let mut past_len: i64 = 0;

        let mut seqs: Vec<Vec<i64>> = vec![Vec::new(); beams];
        let mut beam_scores: Vec<f32> = vec![0.0; beams];
        // Until the first expansion all beams are identical; keep only beam 0 live.
        for s in beam_scores.iter_mut().skip(1) {
            *s = f32::NEG_INFINITY;
        }
        let mut cur_tokens: Vec<i64> = vec![self.meta.decoder_start_token_id; beams];
        let mut finished: Vec<Hypothesis> = Vec::new();

        let limit = self.meta.max_length.min(src_len * 3 + 24);
        let mut steps_run = 0usize;

        for step in 0..limit {
            steps_run += 1;
            let first = step == 0;
            let logits = self.decoder_step(
                &cur_tokens,
                &tiled_dims,
                &tiled_hidden,
                &mask,
                src_len,
                first,
                past_len,
                &self_kv,
                &cross_kv,
                b,
                heads,
                head_dim,
                layers,
            )?;

            let (vocab, flat, present) = logits;
            self_kv = present.0;
            if first {
                cross_kv = present.1;
            }
            past_len = self_kv[0].0.len() as i64 / (b * heads * head_dim);

            // log-softmax per beam, with the pad token banned (bad_words_ids).
            let mut cand: Vec<(f32, usize, i64)> = Vec::with_capacity(beams * 2);
            let mut scored: Vec<(f32, usize, i64)> = Vec::new();
            let live = if first { 1 } else { beams };
            for beam in 0..live {
                if beam_scores[beam] == f32::NEG_INFINITY {
                    continue;
                }
                let row = &flat[beam * vocab..(beam + 1) * vocab];
                let max = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let sum: f32 = row.iter().map(|&x| (x - max).exp()).sum();
                let lse = max + sum.ln();
                for (v, &x) in row.iter().enumerate() {
                    if v as i64 == self.meta.pad_token_id {
                        continue;
                    }
                    scored.push((beam_scores[beam] + (x - lse), beam, v as i64));
                }
            }
            // Keep 2x beams so EOS candidates do not starve the live beams.
            let keep = (beams * 2).min(scored.len());
            scored.select_nth_unstable_by(keep - 1, |a, b| {
                b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
            });
            scored.truncate(keep);
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            cand.extend(scored);

            let mut next_seqs: Vec<Vec<i64>> = Vec::with_capacity(beams);
            let mut next_scores: Vec<f32> = Vec::with_capacity(beams);
            let mut next_tokens: Vec<i64> = Vec::with_capacity(beams);
            let mut reorder: Vec<usize> = Vec::with_capacity(beams);

            for &(score, beam, token) in &cand {
                if next_seqs.len() == beams {
                    break;
                }
                if token == self.meta.eos_token_id {
                    let tokens = seqs[beam].clone();
                    if !tokens.is_empty() {
                        let len_norm = (tokens.len() as f32).powf(self.cfg.length_penalty);
                        finished.push(Hypothesis { tokens, score: score / len_norm });
                        // Keep only the best `beams` hypotheses: the worst of that
                        // bounded pool is the bar a live beam has to clear, and an
                        // unbounded pool would push it down forever.
                        if finished.len() > beams {
                            finished.sort_by(|a, b| {
                                b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
                            });
                            finished.truncate(beams);
                        }
                    }
                } else {
                    let mut s = seqs[beam].clone();
                    s.push(token);
                    next_seqs.push(s);
                    next_scores.push(score);
                    next_tokens.push(token);
                    reorder.push(beam);
                }
            }

            if next_seqs.is_empty() {
                break;
            }
            while next_seqs.len() < beams {
                next_seqs.push(next_seqs[0].clone());
                next_scores.push(f32::NEG_INFINITY);
                next_tokens.push(self.meta.eos_token_id);
                reorder.push(reorder[0]);
            }

            self_kv = reorder_cache(&self_kv, &reorder, b, heads, past_len, head_dim);
            seqs = next_seqs;
            beam_scores = next_scores;
            cur_tokens = next_tokens;

            // early_stopping is false in the model's config: stop only once the best
            // live beam can no longer beat the worst kept hypothesis.
            if finished.len() >= beams {
                let worst = finished
                    .iter()
                    .map(|h| h.score)
                    .fold(f32::INFINITY, f32::min);
                let len_norm = ((seqs[0].len() + 1) as f32).powf(self.cfg.length_penalty);
                let best_possible = beam_scores[0] / len_norm;
                if best_possible <= worst {
                    break;
                }
            }
        }

        if std::env::var("ROSETTA_DEBUG").is_ok() {
            eprintln!(
                "[beam] src={src_len} steps={steps_run}/{limit} finished={} best_len={}",
                finished.len(),
                seqs[0].len()
            );
        }

        if finished.is_empty() {
            // Nothing hit EOS within the budget; fall back to the best live beam.
            let best = beam_scores
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(i, _)| i)
                .unwrap_or(0);
            return Ok(seqs[best].clone());
        }

        finished.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        Ok(finished.remove(0).tokens)
    }

    #[allow(clippy::too_many_arguments)]
    fn decoder_step(
        &mut self,
        cur_tokens: &[i64],
        hs_dims: &[i64],
        hidden: &[f32],
        mask: &[i64],
        src_len: usize,
        first: bool,
        past_len: i64,
        self_kv: &[(Vec<f32>, Vec<f32>)],
        cross_kv: &[(Vec<f32>, Vec<f32>)],
        b: i64,
        heads: i64,
        head_dim: i64,
        layers: usize,
    ) -> Result<(usize, Vec<f32>, (Vec<(Vec<f32>, Vec<f32>)>, Vec<(Vec<f32>, Vec<f32>)>))> {
        let mut inputs: Vec<(Cow<str>, SessionInputValue)> = Vec::new();

        inputs.push(("input_ids".into(), tensor_i64(&[b, 1], cur_tokens.to_vec())?.into()));
        inputs.push((
            "encoder_hidden_states".into(),
            tensor_f32(hs_dims, hidden.to_vec())?.into(),
        ));
        if self.layout.wants_encoder_mask {
            inputs.push((
                "encoder_attention_mask".into(),
                tensor_i64(&[b, src_len as i64], mask.to_vec())?.into(),
            ));
        }
        if self.layout.wants_cache_branch {
            let t = Tensor::from_array((vec![1i64], vec![!first])).map_err(|e| anyhow!("{e}"))?;
            inputs.push(("use_cache_branch".into(), t.into()));
        }

        // The merged graph still requires the past inputs on the first pass; a
        // zero-length self cache and a dummy cross cache satisfy it.
        let self_shape = [b, heads, past_len, head_dim];
        let cross_len = if first { 1 } else { src_len as i64 };
        let cross_shape = [b, heads, cross_len, head_dim];
        let dummy = (b * heads * cross_len * head_dim) as usize;

        for l in 0..layers {
            let (dk, dv) = (self_kv[l].0.clone(), self_kv[l].1.clone());
            let (ck, cv) = if first {
                (vec![0f32; dummy], vec![0f32; dummy])
            } else {
                cross_kv[l].clone()
            };
            let names = self.layout.past_in[l].clone();
            inputs.push((names.0.into(), tensor_f32(&self_shape, dk)?.into()));
            inputs.push((names.1.into(), tensor_f32(&self_shape, dv)?.into()));
            inputs.push((names.2.into(), tensor_f32(&cross_shape, ck)?.into()));
            inputs.push((names.3.into(), tensor_f32(&cross_shape, cv)?.into()));
        }

        let out = self.decoder.run(inputs).map_err(|e| anyhow!("decoder: {e}"))?;

        let (lg_shape, logits) = out["logits"]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow!("logits: {e}"))?;
        let vocab = *lg_shape.last().unwrap() as usize;
        let flat = logits.to_vec();

        let mut next_self = Vec::with_capacity(layers);
        let mut next_cross = Vec::with_capacity(layers);
        for l in 0..layers {
            let n = self.layout.present_out[l].clone();
            let grab = |key: &str| -> Result<Vec<f32>> {
                Ok(out[key]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| anyhow!("{key}: {e}"))?
                    .1
                    .to_vec())
            };
            next_self.push((grab(&n.0)?, grab(&n.1)?));
            if first {
                next_cross.push((grab(&n.2)?, grab(&n.3)?));
            }
        }

        Ok((vocab, flat, (next_self, next_cross)))
    }
}

/// Permute the batch dimension of every self-attention cache tensor so each beam
/// carries the history of the beam it was expanded from.
fn reorder_cache(
    cache: &[(Vec<f32>, Vec<f32>)],
    order: &[usize],
    b: i64,
    heads: i64,
    past_len: i64,
    head_dim: i64,
) -> Vec<(Vec<f32>, Vec<f32>)> {
    let per_beam = (heads * past_len * head_dim) as usize;
    if per_beam == 0 || order.iter().enumerate().all(|(i, &s)| i == s) {
        return cache.to_vec();
    }
    cache
        .iter()
        .map(|(k, v)| {
            let mut nk = Vec::with_capacity(b as usize * per_beam);
            let mut nv = Vec::with_capacity(b as usize * per_beam);
            for &src in order {
                nk.extend_from_slice(&k[src * per_beam..(src + 1) * per_beam]);
                nv.extend_from_slice(&v[src * per_beam..(src + 1) * per_beam]);
            }
            (nk, nv)
        })
        .collect()
}

fn build_session(path: &Path, cfg: &Config) -> Result<(Session, Backend)> {
    let mut builder = Session::builder()
        .map_err(|e| anyhow!("{e}"))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| anyhow!("{e}"))?
        .with_intra_threads(cfg.threads)
        .map_err(|e| anyhow!("{e}"))?;

    let mut active = Backend::Cpu;
    match cfg.backend {
        Backend::Cpu => {}
        Backend::DirectMl => {
            // DirectML cannot use ORT's memory-pattern planner and requires
            // sequential execution.
            builder = builder
                .with_memory_pattern(false)
                .map_err(|e| anyhow!("{e}"))?
                .with_parallel_execution(false)
                .map_err(|e| anyhow!("{e}"))?;
            match builder
                .clone()
                .with_execution_providers([ort::ep::DirectML::default().build().error_on_failure()])
            {
                Ok(b) => {
                    builder = b;
                    active = Backend::DirectMl;
                }
                Err(e) => eprintln!("[warn] DirectML unavailable ({e}); falling back to CPU"),
            }
        }
        Backend::Cuda => {
            #[cfg(feature = "cuda")]
            match builder
                .clone()
                .with_execution_providers([ort::ep::CUDA::default().build().error_on_failure()])
            {
                Ok(b) => {
                    builder = b;
                    active = Backend::Cuda;
                }
                Err(e) => eprintln!("[warn] CUDA unavailable ({e}); falling back to CPU"),
            }
            #[cfg(not(feature = "cuda"))]
            eprintln!("[warn] built without the `cuda` feature; falling back to CPU");
        }
    }

    let session = builder
        .commit_from_file(path)
        .map_err(|e| anyhow!("loading {}: {e}", path.display()))?;
    Ok((session, active))
}

fn tensor_i64(shape: &[i64], data: Vec<i64>) -> Result<Tensor<i64>> {
    Tensor::from_array((shape.to_vec(), data)).map_err(|e| anyhow!("{e}"))
}

fn tensor_f32(shape: &[i64], data: Vec<f32>) -> Result<Tensor<f32>> {
    Tensor::from_array((shape.to_vec(), data)).map_err(|e| anyhow!("{e}"))
}
