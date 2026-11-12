//! AniSkip (https://aniskip.com): where an episode's opening is, for a rip
//! whose chapters don't say. Free, and no key is needed. It knows anime by
//! MyAnimeList's ID, which AniList gives.
//!
//!   GET /v2/skip-times/{MyAnimeList ID}/{episode}?types=op&episodeLength={seconds}
//!
//! People mark the times on a video of some length. Another cut of an episode
//! can put the opening elsewhere, so Telly asks only for times marked on a
//! video within 20 seconds of the rip's length (AniSkip's own leeway). It
//! answers 120 requests a minute.

use crate::anilist;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;

const API: &str = "https://api.aniskip.com/v2";
const USER_AGENT: &str = concat!("Telly/", env!("CARGO_PKG_VERSION"), " (personal anime player)");
/// The time between requests that keeps to AniSkip's 120 a minute.
pub const PACE: Duration = Duration::from_millis(500);

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

/// MyAnimeList's ID for the anime AniList knows as `anilist_id`. None: AniList has none.
pub fn mal_id(anilist_id: i64) -> Result<Option<i64>> {
    let answer = anilist::query_patiently("query ($id: Int) { Media(id: $id, type: ANIME) { idMal } }", json!({ "id": anilist_id }))?;
    Ok(answer.pointer("/data/Media/idMal").and_then(Value::as_i64))
}

/// Where the opening of episode `number` is, as (start, end) in seconds, in a
/// video `length` seconds long. None: AniSkip has none for it.
pub fn opening(mal_id: i64, number: i64, length: f64) -> Result<Option<(f64, f64)>> {
    let url = format!("{API}/skip-times/{mal_id}/{number}?types=op&episodeLength={length:.3}");
    let mut response = agent().get(&url).header("User-Agent", USER_AGENT).call().map_err(|e| anyhow!("AniSkip isn’t reachable ({e})"))?;
    match response.status().as_u16() {
        200 => {}
        404 => return Ok(None),
        status => return Err(anyhow!("AniSkip answered {status}")),
    }
    let answer: Value = response.body_mut().read_json().context("AniSkip sent something unexpected")?;
    Ok(first_opening(&answer))
}

/// The opening in AniSkip's answer: its first, the one with the most votes.
fn first_opening(answer: &Value) -> Option<(f64, f64)> {
    let op = answer["results"].as_array()?.iter().find(|r| r["skipType"] == "op")?;
    let start = op.pointer("/interval/startTime")?.as_f64()?;
    let end = op.pointer("/interval/endTime")?.as_f64()?;
    (end > start).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_opening_from_an_answer() {
        // AniSkip's answer for Frieren's first episode in a 1,500-second
        // video (12 November 2026), its ending put first.
        let answer = json!({
            "found": true,
            "results": [
                { "interval": { "startTime": 1417.135, "endTime": 1507.135 }, "skipType": "ed", "skipId": "92965d0e", "episodeLength": 1510.102 },
                { "interval": { "startTime": 116.748, "endTime": 206.748 }, "skipType": "op", "skipId": "657f5b16", "episodeLength": 1510.092 }
            ],
            "message": "Successfully found skip times",
            "statusCode": 200
        });
        assert_eq!(first_opening(&answer), Some((116.748, 206.748)));
        assert_eq!(first_opening(&json!({ "found": false, "results": [], "statusCode": 404 })), None);
    }
}
