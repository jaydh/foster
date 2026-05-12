use foster_core::{MachineBuilder, MachineError};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

// Mirror of main.rs reducers — gen_tests is a standalone binary.
fn load_track(_: Value, _: Value) -> Result<Value, MachineError> {
    Ok(json!({ "title": "", "artist": "", "position": 0, "duration": 213, "error": "" }))
}
fn seek_forward(c: Value, _: Value) -> Result<Value, MachineError> {
    let pos = c["position"].as_i64().unwrap_or(0);
    let dur = c["duration"].as_i64().unwrap_or(0);
    let mut m = c.as_object().cloned().unwrap_or_default();
    m.insert("position".into(), json!((pos + 10).min(dur)));
    Ok(Value::Object(m))
}
fn seek_back(c: Value, _: Value) -> Result<Value, MachineError> {
    let pos = c["position"].as_i64().unwrap_or(0);
    let mut m = c.as_object().cloned().unwrap_or_default();
    m.insert("position".into(), json!((pos - 10).max(0)));
    Ok(Value::Object(m))
}
fn set_ended(c: Value, _: Value) -> Result<Value, MachineError> {
    let dur = c["duration"].as_i64().unwrap_or(0);
    let mut m = c.as_object().cloned().unwrap_or_default();
    m.insert("position".into(), json!(dur));
    Ok(Value::Object(m))
}
fn reset_position(c: Value, _: Value) -> Result<Value, MachineError> {
    let mut m = c.as_object().cloned().unwrap_or_default();
    m.insert("position".into(), json!(0));
    Ok(Value::Object(m))
}
fn set_error(c: Value, p: Value) -> Result<Value, MachineError> {
    let msg = p["message"].as_str().unwrap_or("Playback failed").to_string();
    let mut m = c.as_object().cloned().unwrap_or_default();
    m.insert("error".into(), json!(msg));
    Ok(Value::Object(m))
}
fn clear_error(c: Value, _: Value) -> Result<Value, MachineError> {
    let mut m = c.as_object().cloned().unwrap_or_default();
    m.insert("error".into(), json!(""));
    Ok(Value::Object(m))
}

fn main() {
    let machine = MachineBuilder::new(
        "player",
        "idle",
        json!({ "title": "", "artist": "", "position": 0, "duration": 0, "error": "" }),
    )
    .state("loading").state("playing").state("paused").state("ended").state("error")
    .on("idle",    "load",       "loading", Some(load_track))
    .on("loading", "ready",      "playing", None)
    .on("loading", "fail",       "error",   Some(set_error))
    .on("playing", "pause",      "paused",  None)
    .on("playing", "forward_10", "playing", Some(seek_forward))
    .on("playing", "back_10",    "playing", Some(seek_back))
    .on("playing", "end",        "ended",   Some(set_ended))
    .on("paused",  "play",       "playing", None)
    .on("paused",  "forward_10", "paused",  Some(seek_forward))
    .on("paused",  "back_10",    "paused",  Some(seek_back))
    .on("ended",   "replay",     "playing", Some(reset_position))
    .on("ended",   "load",       "loading", Some(load_track))
    .on("error",   "retry",      "loading", Some(clear_error))
    .on("error",   "dismiss",    "idle",    None)
    .build();

    let base_url  = "http://localhost:3001";
    let out_dir   = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    let spec_path = format!("{out_dir}/player.spec.ts");
    let cfg_path  = concat!(env!("CARGO_MANIFEST_DIR"), "/playwright.config.ts");

    fs::create_dir_all(out_dir).unwrap();
    fs::write(&spec_path, foster_testgen::generate(&machine, base_url)).unwrap();
    println!("wrote  {spec_path}");

    if !Path::new(cfg_path).exists() {
        fs::write(cfg_path, foster_testgen::generate_playwright_config(base_url)).unwrap();
        println!("wrote  {cfg_path}");
    }
}
