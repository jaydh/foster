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
//! that, with four other Foster machines already on it, is now sitting right
//! at the six-connections-per-origin HTTP/1.1 limit Foster's own README
//! warns about.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
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

    // Simulate the real screening job: CPU/IO work that takes a few seconds
    // and finishes on its own, independent of any further client request.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let mut s = state.lock().await;
        *s = Screening::Complete {
            events: vec![
                "ISS (ZARYA) vs COSMOS 2251 DEB — miss distance 4.2 km".to_string(),
                "STARLINK-3011 vs FENGYUN 1C DEB — miss distance 1.8 km".to_string(),
            ],
        };
    });

    StatusCode::ACCEPTED
}
