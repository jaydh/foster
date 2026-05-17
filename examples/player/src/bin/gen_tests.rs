use foster_core::MachineBuilder;
use serde_json::json;
use std::fs;
use std::path::Path;

fn main() {
    let machine = MachineBuilder::new(
        "player",
        "idle",
        json!({ "title": "", "artist": "", "position": 0, "duration": 0, "error": "" }),
    )
    .state("loading").state("playing").state("paused").state("ended").state("error")
    .pass("idle",    "load",       "loading")
    .pass("loading", "ready",      "playing")
    .pass("loading", "fail",       "error")
    .pass("playing", "pause",      "paused")
    .pass("playing", "forward_10", "playing")
    .pass("playing", "back_10",    "playing")
    .pass("playing", "end",        "ended")
    .pass("paused",  "play",       "playing")
    .pass("paused",  "forward_10", "paused")
    .pass("paused",  "back_10",    "paused")
    .pass("ended",   "replay",     "playing")
    .pass("ended",   "load",       "loading")
    .pass("error",   "retry",      "loading")
    .pass("error",   "dismiss",    "idle")
    .build();

    let base_url  = "http://localhost:3001";
    let out_dir   = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    let spec_path = format!("{out_dir}/player.spec.ts");
    let cfg_path  = concat!(env!("CARGO_MANIFEST_DIR"), "/playwright.config.ts");

    let sdk_path = format!("{out_dir}/player.sdk.ts");

    fs::create_dir_all(out_dir).unwrap();
    fs::write(&spec_path, foster_testgen::generate(&machine, base_url)).unwrap();
    println!("  {}", foster_testgen::summary(&machine));
    println!("wrote  {spec_path}");

    fs::write(&sdk_path, foster_testgen::generate_sdk(&machine, base_url)).unwrap();
    println!("wrote  {sdk_path}");

    if !Path::new(cfg_path).exists() {
        fs::write(cfg_path, foster_testgen::generate_playwright_config(base_url, "player")).unwrap();
        println!("wrote  {cfg_path}");
    }
}
