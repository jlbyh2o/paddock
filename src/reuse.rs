//! Estimating how much of each prompt the engine served from its prefix cache.
//!
//! FreeToken reports this exactly — but only in the `usage.prompt_tokens_details`
//! block of a completion response, which paddock never sees: it polls the control plane
//! and does not proxy model traffic. Neither `/v1/stats` nor the request ring carries a
//! cached-token count, so the number has to be inferred from what the ring does report.
//!
//! The inference rests on one fact: time-to-first-token is dominated by prefill, and
//! prefill only runs over the part of the prompt that was *not* already cached. A 70k
//! prompt that reaches first token in 0.8 s on hardware that prefills ~3k tokens/s cannot
//! have prefilled 70k tokens — it prefilled about 2k and read the rest from the cache.
//!
//! The one unknown is that cold prefill rate, and it is learned rather than assumed:
//! across a spread of requests, the *slowest* observed tokens-per-second is the closest
//! thing to an uncached run, so it anchors the scale. That makes the estimate
//! deliberately conservative — if every request in the window was partly cached, the
//! anchor is too fast and the reported reuse is too low. Better to understate a number
//! this soft than to overstate it.
//!
//! The estimate has a known bias worth stating: prefill runs *faster per token* on short
//! prompts than long ones (measured on one RTX 5070 Ti: ~3,400 tok/s at 4k against
//! ~1,900 tok/s at 246k). A single cold rate cannot capture that, so a window mixing very
//! short and very long prompts will read the short ones as more cached than they were. In
//! the case this exists for — one agent session whose prompts are all large and grow
//! slowly — the sizes cluster and the bias is small.
//!
//! Two guards keep it from reporting nonsense. Requests below [`MIN_PROMPT_TOKENS`] are
//! ignored, because at small sizes TTFT is mostly request overhead rather than prefill.
//! And unless the window contains both a slow and a fast request ([`MIN_SPREAD`]), no
//! estimate is produced at all: uniformly fast requests are equally consistent with "every
//! prompt was cached" and "this GPU is quick", and there is no way to tell them apart from
//! outside. A missing answer is the honest one there.

use crate::ft::types::RequestRecord;
use serde::Serialize;

/// Below this, time-to-first-token is dominated by queueing and request overhead rather
/// than prefill, and the implied rate is meaningless.
pub const MIN_PROMPT_TOKENS: u64 = 2048;

/// Fewer usable samples than this and the anchor is one request's noise.
pub const MIN_SAMPLES: usize = 3;

/// The fastest sample must beat the slowest by this much before the spread is taken as
/// evidence of caching rather than of ordinary variation.
pub const MIN_SPREAD: f64 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Reuse {
    /// Fraction of prompt tokens served from cache, 0.0 to 1.0.
    pub fraction: f64,
    /// The cold prefill rate the estimate was anchored on, in tokens per second.
    pub cold_rate: f64,
    pub samples: usize,
}

impl Reuse {
    /// A short phrase for a status line, always marked as an estimate so it is never
    /// mistaken for the engine's own accounting.
    pub fn summary(&self) -> String {
        format!("~{:.0}%  (est. from {} reqs)", self.fraction * 100.0, self.samples)
    }
}

/// One usable observation: a streamed request big enough for its TTFT to mean something.
fn sample(r: &RequestRecord) -> Option<(u64, f64)> {
    let tokens = r.prompt_tokens?;
    // Non-streaming requests report no TTFT at all, so they cannot be used.
    let ttft = r.ttft_ms?;
    if tokens < MIN_PROMPT_TOKENS || ttft == 0 || r.status >= 400 {
        return None;
    }
    Some((tokens, ttft as f64 / 1000.0))
}

/// Estimate prefix reuse across the requests in `entries`.
///
/// `None` when there is not enough evidence — too few usable requests, or no spread
/// between them. The caller shows nothing rather than a fabricated percentage.
pub fn estimate(entries: &[RequestRecord]) -> Option<Reuse> {
    let samples: Vec<(u64, f64)> = entries.iter().filter_map(sample).collect();
    if samples.len() < MIN_SAMPLES {
        return None;
    }

    // Apparent throughput per request. A cached prompt looks impossibly fast; an uncached
    // one runs at the hardware's real rate.
    let rates: Vec<f64> = samples.iter().map(|(tok, s)| *tok as f64 / s).collect();
    let cold = rates.iter().cloned().fold(f64::INFINITY, f64::min);
    let hot = rates.iter().cloned().fold(0.0, f64::max);
    if !cold.is_finite() || cold <= 0.0 || hot / cold < MIN_SPREAD {
        return None;
    }

    // Price every prompt against the cold anchor: whatever the elapsed prefill time could
    // not have covered must have come from the cache.
    let mut cached = 0.0;
    let mut total = 0.0;
    for (tokens, ttft) in &samples {
        let prefilled = (ttft * cold).min(*tokens as f64);
        cached += *tokens as f64 - prefilled;
        total += *tokens as f64;
    }
    if total <= 0.0 {
        return None;
    }
    Some(Reuse {
        fraction: (cached / total).clamp(0.0, 1.0),
        cold_rate: cold,
        samples: samples.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(prompt: u64, ttft_ms: u64) -> RequestRecord {
        RequestRecord {
            status: 200,
            prompt_tokens: Some(prompt),
            ttft_ms: Some(ttft_ms),
            ..Default::default()
        }
    }

    #[test]
    fn a_heavily_cached_session_reports_high_reuse() {
        // One cold 70k request at ~3k tok/s, then the same prompt served from cache.
        let entries =
            vec![req(70_000, 23_300), req(70_100, 780), req(70_500, 790), req(71_000, 800)];
        let r = estimate(&entries).expect("a clear spread should estimate");
        assert!(r.fraction > 0.7, "expected heavy reuse, got {:.2}", r.fraction);
        assert!((r.cold_rate - 3004.0).abs() < 50.0, "anchor {:.0}", r.cold_rate);
    }

    #[test]
    fn the_slowest_request_anchors_the_scale_and_scores_zero_itself() {
        let entries = vec![req(60_000, 20_000), req(60_000, 1_000), req(60_000, 1_000)];
        let r = estimate(&entries).unwrap();
        // The anchor request contributes no cached tokens by construction.
        assert_eq!(r.cold_rate, 3000.0);
        // Two of three were ~95% cached, so the mean lands near two thirds.
        assert!(0.55 < r.fraction && r.fraction < 0.70, "{:.2}", r.fraction);
    }

    #[test]
    fn uniform_requests_yield_no_estimate_because_they_are_ambiguous() {
        // All fast. Could be a cached session or a quick GPU; nothing here can tell.
        let entries = vec![req(50_000, 1_000), req(50_000, 1_010), req(50_000, 990)];
        assert!(estimate(&entries).is_none(), "no spread means no honest answer");
    }

    #[test]
    fn too_few_usable_requests_yield_no_estimate() {
        assert!(estimate(&[]).is_none());
        assert!(estimate(&[req(50_000, 20_000), req(50_000, 500)]).is_none());
    }

    #[test]
    fn small_prompts_are_ignored_because_ttft_is_mostly_overhead() {
        let entries = vec![req(100, 200), req(150, 210), req(120, 190), req(80, 205)];
        assert!(estimate(&entries).is_none(), "sub-2k prompts must not anchor anything");
    }

    #[test]
    fn non_streaming_requests_are_skipped_rather_than_counted_as_instant() {
        // ttft is null for a non-streamed request; treating that as 0 would imply
        // infinite reuse.
        let mut r = req(50_000, 0);
        r.ttft_ms = None;
        let entries = vec![r, req(50_000, 20_000), req(50_000, 700), req(50_000, 710)];
        let est = estimate(&entries).unwrap();
        assert_eq!(est.samples, 3, "the non-streamed request should not be a sample");
    }

    #[test]
    fn failed_requests_do_not_pollute_the_anchor() {
        let mut bad = req(50_000, 5);
        bad.status = 500;
        let entries = vec![bad, req(50_000, 20_000), req(50_000, 700), req(50_000, 700)];
        let est = estimate(&entries).unwrap();
        assert_eq!(est.samples, 3);
        assert_eq!(est.cold_rate, 2500.0, "a 5 ms error must not become the cold rate");
    }

    #[test]
    fn reuse_never_exceeds_one_or_falls_below_zero() {
        let entries = vec![req(200_000, 60_000), req(200_000, 1), req(200_000, 2)];
        let r = estimate(&entries).unwrap();
        assert!((0.0..=1.0).contains(&r.fraction));
    }

    #[test]
    fn the_summary_is_marked_as_an_estimate() {
        let r = Reuse { fraction: 0.968, cold_rate: 2950.0, samples: 12 };
        assert_eq!(r.summary(), "~97%  (est. from 12 reqs)");
    }
}
