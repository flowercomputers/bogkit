//! Precision rewrites: the AI writes only where you point, and the lenses
//! check its work.
//!
//! The instrument does the targeting (which sentence, which direction — an
//! axis pole or a specific exemplar voice) and the verification (re-embed
//! with ese, re-score with the same Axis the paint uses, measure movement).
//! The language model is a guest: it drafts, we audit. A draft that doesn't
//! move the needle gets one retry with its own score as feedback; the writer
//! sees the receipt either way and accepting is an ordinary edit — one fold
//! upsert, every view updates.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

use crate::serve::Axis;

#[derive(Debug, Deserialize)]
pub struct RestyleReq {
    pub text: String,
    #[serde(default)]
    pub passage: String,
    /// "axis" (dir = "concrete" | "abstract") or "voice" (target_* set)
    pub mode: String,
    #[serde(default)]
    pub dir: String,
    #[serde(default)]
    pub target_text: String,
    #[serde(default)]
    pub target_title: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Metrics {
    pub t: f32,
    pub dist: Option<f32>,
}

#[derive(Debug, Serialize)]
pub struct RestyleResp {
    pub status: String, // ok | no-provider | error
    pub proposal: String,
    pub before: Metrics,
    pub after: Metrics,
    pub verified: bool,
    pub attempts: u32,
    pub model: String,
    pub note: String,
}

fn cos_dist(a: &[f32], b: &[f32]) -> f32 {
    1.0 - crate::dot(a, b) / (crate::norm(a) * crate::norm(b)).max(1e-9)
}

/// OPENAI_API_KEY from the environment, then the Loupe-era env files. Values
/// may be quoted; the last line of a dotenv may lack a newline — both handled.
pub fn api_key() -> Option<String> {
    if let Ok(k) = std::env::var("OPENAI_API_KEY")
        && !k.trim().is_empty()
    {
        return Some(k.trim().to_string());
    }
    let home = std::env::var("HOME").ok()?;
    for path in [format!("{home}/.loupe-env"), format!("{home}/Documents/GitHub/loupe/.env")] {
        let Ok(txt) = std::fs::read_to_string(&path) else { continue };
        for line in txt.lines() {
            if let Some((k, v)) = line.split_once('=')
                && k.trim() == "OPENAI_API_KEY"
            {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

pub fn model_name() -> String {
    std::env::var("SOUNDINGS_LLM_MODEL")
        .or_else(|_| std::env::var("LLM_MODEL"))
        .unwrap_or_else(|_| "gpt-5.6-sol".into())
}

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(45))
            .build()
            .unwrap()
    })
}

async fn draft(key: &str, model: &str, sys: &str, user: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "model": model,
        "max_completion_tokens": 2000,
        "messages": [
            {"role": "system", "content": sys},
            {"role": "user", "content": user}
        ]
    });
    let r = client()
        .post("https://api.openai.com/v1/chat/completions")
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let v: serde_json::Value = r.json().await.map_err(|e| e.to_string())?;
    let content = v["choices"][0]["message"]["content"].as_str().unwrap_or("").trim();
    if content.is_empty() {
        return Err(v["error"]["message"].as_str().unwrap_or("empty model response").to_string());
    }
    // one sentence, no wrapping quotes, no stray newlines
    Ok(content.trim_matches('"').trim_matches('\u{201c}').trim_matches('\u{201d}').replace('\n', " ").trim().to_string())
}

const SYS: &str = "You are a precision line editor inside a writing instrument. Revise EXACTLY ONE sentence. Preserve its meaning, facts, and narrative content. Change only its style. Return only the revised sentence — no quotes, no commentary.";

pub async fn run(req: RestyleReq) -> RestyleResp {
    let axis = Axis::concrete_abstract();
    let target_vec = (req.mode == "voice").then(|| crate::embed(&req.target_text));
    let measure = |text: &str| Metrics {
        t: axis.score(text).t,
        dist: target_vec.as_ref().map(|tv| cos_dist(&crate::embed(text), tv)),
    };
    let before = measure(&req.text);
    let fail = |status: &str, note: String, before: &Metrics| RestyleResp {
        status: status.into(),
        proposal: String::new(),
        before: before.clone(),
        after: before.clone(),
        verified: false,
        attempts: 0,
        model: model_name(),
        note,
    };
    let Some(key) = api_key() else {
        return fail("no-provider", "no OPENAI_API_KEY found (env, ~/.loupe-env, loupe/.env) — the targeting and receipts work; only drafting needs a model".into(), &before);
    };
    let model = model_name();

    let task = match req.mode.as_str() {
        "voice" => format!(
            "Task: revise the sentence to move closer to the voice of this exemplar from {} — its cadence and diction, not its content:\n“{}”",
            if req.target_title.is_empty() { "the canon" } else { &req.target_title },
            req.target_text
        ),
        _ if req.dir == "abstract" => "Task: revise the sentence to be noticeably more abstract — conceptual, general, idea-level. (The instrument's abstract pole is anchored by sentences like “Freedom is the capacity to author one's own life.”)".to_string(),
        _ => "Task: revise the sentence to be noticeably more concrete — physical, sensory, specific. (The instrument's concrete pole is anchored by sentences like “She wiped the counter and stacked the blue ceramic bowls.”)".to_string(),
    };
    let base = format!(
        "Sentence to revise:\n{}\n\nIts paragraph, for context:\n{}\n\n{}",
        req.text,
        if req.passage.is_empty() { req.text.clone() } else { req.passage.clone() },
        task
    );

    let moved = |after: &Metrics| match req.mode.as_str() {
        "voice" => after.dist.unwrap_or(1.0) < before.dist.unwrap_or(1.0) - 0.015,
        _ if req.dir == "abstract" => after.t > before.t + 0.02,
        _ => after.t < before.t - 0.02,
    };

    let mut prompt = base.clone();
    let mut last = (String::new(), before.clone());
    for attempt in 1..=2u32 {
        match draft(&key, &model, SYS, &prompt).await {
            Ok(p) => {
                let after = measure(&p);
                let ok = moved(&after);
                last = (p, after);
                if ok {
                    return RestyleResp {
                        status: "ok".into(),
                        proposal: last.0,
                        before,
                        after: last.1,
                        verified: true,
                        attempts: attempt,
                        model,
                        note: String::new(),
                    };
                }
                // meter-guided retry: the draft's own score is the feedback
                prompt = format!(
                    "{base}\n\nYour previous draft was:\n{}\nThe instrument measured it and it did not move far enough in the requested direction. Go noticeably further while preserving the meaning.",
                    last.0
                );
            }
            Err(e) => return fail("error", e, &before),
        }
    }
    RestyleResp {
        status: "ok".into(),
        proposal: last.0,
        before,
        after: last.1,
        verified: false,
        attempts: 2,
        model,
        note: "did not move the needle — shown anyway, your call".into(),
    }
}
