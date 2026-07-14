//! Real Lighthouse audit against this demo's own running server — same
//! background-job + independent-polling shape as screening.rs
//! (conjunction), not a Foster machine for the run itself: the audit takes
//! 15-30s of real Chrome-headless work, unrelated to any discrete UI state.
//! Foster only owns the button label (idle/started), matching conjunction's
//! pattern exactly.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::Serialize;
use std::process::Command;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum LighthouseRun {
    Idle,
    Running,
    #[serde(rename_all = "camelCase")]
    Complete {
        performance: f64,
        accessibility: f64,
        best_practices: f64,
        seo: f64,
        report_url: String,
    },
    Failed {
        error: String,
    },
}

pub type LighthouseState = Arc<Mutex<LighthouseRun>>;

pub fn initial_state() -> LighthouseState {
    Arc::new(Mutex::new(LighthouseRun::Idle))
}

pub async fn get_lighthouse(State(state): State<LighthouseState>) -> Json<LighthouseRun> {
    Json(state.lock().await.clone())
}

pub async fn start_lighthouse(State(state): State<LighthouseState>) -> StatusCode {
    {
        let mut s = state.lock().await;
        *s = LighthouseRun::Running;
    }

    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(run_lighthouse_blocking)
            .await
            .unwrap_or_else(|e| Err(e.to_string()));

        let mut s = state.lock().await;
        *s = match result {
            Ok(scores) => scores,
            Err(error) => LighthouseRun::Failed { error },
        };
    });

    StatusCode::ACCEPTED
}

/// chrome-launcher's own Chrome auto-detection can fail when spawned as a
/// child of a background/non-interactive process (reproduced locally: works
/// fine run directly in a terminal, fails with "Unable to connect to
/// Chrome" when spawned from inside the axum server process) — pointing it
/// at a known browser explicitly sidesteps the detection entirely.
fn find_chrome_path() -> Option<&'static str> {
    const CANDIDATES: &[&str] = &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
    ];
    CANDIDATES
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .copied()
}

fn run_lighthouse_blocking() -> Result<LighthouseRun, String> {
    let static_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/static");
    let json_path = format!("{static_dir}/lighthouse-report.json");
    let html_path = format!("{static_dir}/lighthouse-report.html");
    let chrome_path = find_chrome_path();

    let mut json_cmd = Command::new("npx");
    json_cmd.args([
        "--yes",
        "lighthouse",
        "http://localhost:3009/",
        "--output=json",
        &format!("--output-path={json_path}"),
        "--chrome-flags=--headless --no-sandbox",
        "--quiet",
    ]);
    if let Some(p) = chrome_path {
        json_cmd.env("CHROME_PATH", p);
    }
    let json_status = json_cmd
        .status()
        .map_err(|e| format!("failed to spawn lighthouse: {e}"))?;

    if !json_status.success() {
        return Err(format!("lighthouse exited with {json_status}"));
    }

    // Also generate the human-viewable HTML report.
    let mut html_cmd = Command::new("npx");
    html_cmd.args([
        "--yes",
        "lighthouse",
        "http://localhost:3009/",
        "--output=html",
        &format!("--output-path={html_path}"),
        "--chrome-flags=--headless --no-sandbox",
        "--quiet",
    ]);
    if let Some(p) = chrome_path {
        html_cmd.env("CHROME_PATH", p);
    }
    let _ = html_cmd.status();

    let json_text = std::fs::read_to_string(&json_path)
        .map_err(|e| format!("failed to read lighthouse report: {e}"))?;
    let report: serde_json::Value =
        serde_json::from_str(&json_text).map_err(|e| format!("failed to parse report: {e}"))?;

    let score = |cat: &str| -> f64 {
        report["categories"][cat]["score"].as_f64().unwrap_or(0.0) * 100.0
    };

    Ok(LighthouseRun::Complete {
        performance: score("performance"),
        accessibility: score("accessibility"),
        best_practices: score("best-practices"),
        seo: score("seo"),
        report_url: "/lighthouse-report.html".to_string(),
    })
}
