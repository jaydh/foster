//! Real `cargo audit` run against the `foster` workspace's own `Cargo.lock`
//! — the exact tool used earlier this session to find and fix
//! RUSTSEC-2026-0204 in `crossbeam-epoch`. Self-referential: whatever this
//! section reports is genuinely, currently true of this repository right
//! now, not sample data.

use serde_json::Value;
use std::process::Command;

pub fn run_audit() -> Value {
    // `cargo audit` does its own blocking subprocess spawn + advisory-db
    // fetch; block_in_place hands the tokio worker thread back to the
    // scheduler for the duration, same reasoning as the kube/Postgres calls
    // in cluster.rs/visitors.rs (and see store.rs for why that reasoning
    // matters: a reducer that blocks for a few seconds must not also be
    // holding a lock that stalls every other machine on the page).
    let result = tokio::task::block_in_place(|| {
        Command::new("cargo")
            .args(["audit", "--json"])
            .current_dir(workspace_root())
            .output()
    });

    let output = match result {
        Ok(o) => o,
        Err(e) => {
            return serde_json::json!({
                "ran": false,
                "error": e.to_string(),
                "vulnerability_count": 0,
                "vulnerabilities": [],
                "warning_count": 0,
                "dependency_count": 0,
            });
        }
    };

    // cargo-audit exits non-zero when it finds vulnerabilities — that's
    // expected and not a failure to run; the JSON on stdout is still valid.
    let parsed: Option<Value> = serde_json::from_slice(&output.stdout).ok();

    match parsed {
        Some(report) => {
            let vulns: Vec<Value> = report["vulnerabilities"]["list"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|v| {
                    serde_json::json!({
                        "id": v["advisory"]["id"],
                        "package": v["advisory"]["package"],
                        "title": v["advisory"]["title"],
                    })
                })
                .collect();

            let warning_count: i64 = report["warnings"]
                .as_object()
                .map(|w| w.values().map(|v| v.as_array().map(|a| a.len()).unwrap_or(0) as i64).sum())
                .unwrap_or(0);

            serde_json::json!({
                "ran": true,
                "vulnerability_count": vulns.len(),
                "vulnerabilities": vulns,
                "warning_count": warning_count,
                "dependency_count": report["lockfile"]["dependency-count"],
            })
        }
        None => serde_json::json!({
            "ran": false,
            "error": String::from_utf8_lossy(&output.stderr).to_string(),
            "vulnerability_count": 0,
            "vulnerabilities": [],
            "warning_count": 0,
            "dependency_count": 0,
        }),
    }
}

fn workspace_root() -> std::path::PathBuf {
    // examples/jaydanhoward/src/.. /.. /.. → foster workspace root
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
}
