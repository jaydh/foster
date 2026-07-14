//! Conjunction-screening analog: a background job that runs independently of
//! any client request, polled by a widget that has nothing to do with
//! Foster's own SSE stream.
//!
//! This is deliberately built as a *separate*, hand-rolled axum route pair
//! (`GET /api/screening`, `POST /api/screening/start`) rather than a Foster
//! machine, because the thing being tested is whether Foster's own SSE
//! connection for the "conjunction" machine (open the whole time this
//! section is on the page) interferes with a widget's *independent* polling
//! loop against a *different* endpoint — both running concurrently on a page
//! that, with several other Foster machines already on it, sits right at
//! (or past) the six-connections-per-origin HTTP/1.1 limit Foster's own
//! README warns about.
//!
//! The screening pass itself is real, if scoped down from the real site's
//! full multi-timestep Hoots-filter/SGP4 screening (`conjunction.rs`,
//! 2135 lines): it propagates the real "stations" TLE group (see
//! satellites.rs, same real CelesTrak data as the Satellites section) to
//! real ECI positions at the current moment and reports the actual closest
//! real pairs by real distance — not a binary conjunction/no-conjunction
//! call against a threshold (today's real satellites in this small group
//! are well-separated; forcing a "found!" would mean faking the result).

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Screening {
    Idle,
    Running,
    Complete { events: Vec<String> },
}

pub type ScreeningState = Arc<Mutex<Screening>>;

pub fn initial_state() -> ScreeningState {
    Arc::new(Mutex::new(Screening::Idle))
}

pub async fn get_screening(State(state): State<ScreeningState>) -> Json<Screening> {
    Json(state.lock().await.clone())
}

pub async fn start_screening(State(state): State<ScreeningState>) -> StatusCode {
    {
        let mut s = state.lock().await;
        *s = Screening::Running;
    }

    // Real work that finishes on its own — a real TLE fetch + real pairwise
    // distance computation, independent of any further client request.
    tokio::spawn(async move {
        let events = tokio::task::spawn_blocking(run_real_screening)
            .await
            .unwrap_or_default();
        let mut s = state.lock().await;
        *s = Screening::Complete { events };
    });

    StatusCode::ACCEPTED
}

fn run_real_screening() -> Vec<String> {
    let sats = crate::satellites::fetch_group_positions("stations");

    let mut pairs: Vec<(f64, &str, &str)> = Vec::new();
    for i in 0..sats.len() {
        for j in (i + 1)..sats.len() {
            let (name_a, pos_a) = &sats[i];
            let (name_b, pos_b) = &sats[j];
            let dx = pos_a[0] - pos_b[0];
            let dy = pos_a[1] - pos_b[1];
            let dz = pos_a[2] - pos_b[2];
            let distance_km = (dx * dx + dy * dy + dz * dz).sqrt();
            pairs.push((distance_km, name_a, name_b));
        }
    }
    pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    pairs
        .into_iter()
        .take(5)
        .map(|(distance_km, a, b)| {
            format!("{a} vs {b} — real distance right now: {distance_km:.1} km")
        })
        .collect()
}
