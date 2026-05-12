/// Generate Playwright test scaffolding for the counter machine.
///
/// Usage:
///   cargo run -p counter --bin gen_tests
///
/// Writes:
///   examples/counter/tests/counter.spec.ts
///   examples/counter/playwright.config.ts  (only if it does not already exist)
use foster_core::{MachineBuilder, MachineError};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

fn increment(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let n = ctx["count"].as_i64().unwrap_or(0);
    Ok(json!({ "count": n + 1 }))
}

fn decrement(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let n = ctx["count"].as_i64().unwrap_or(0);
    Ok(json!({ "count": n - 1 }))
}

fn reset(_: Value, _: Value) -> Result<Value, MachineError> {
    Ok(json!({ "count": 0 }))
}

fn passthrough(ctx: Value, _: Value) -> Result<Value, MachineError> {
    Ok(ctx)
}

fn main() {
    let machine = MachineBuilder::new("counter", "idle", json!({ "count": 0 }))
        .state("error")
        .on("idle", "increment", "idle", Some(increment))
        .on("idle", "decrement", "idle", Some(decrement))
        .on("idle", "reset", "idle", Some(reset))
        .on("idle", "break_it", "error", Some(passthrough))
        .on("error", "recover", "idle", Some(passthrough))
        .build();

    let base_url = "http://localhost:3000";
    let out_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    let spec_path = format!("{out_dir}/counter.spec.ts");
    let config_path = concat!(env!("CARGO_MANIFEST_DIR"), "/playwright.config.ts");

    fs::create_dir_all(out_dir).unwrap();

    let spec = foster_testgen::generate(&machine, base_url);
    fs::write(&spec_path, &spec).unwrap();
    println!("wrote  {spec_path}");

    // Only create the config if it doesn't exist so manual edits are preserved.
    if !Path::new(config_path).exists() {
        let config = foster_testgen::generate_playwright_config(base_url);
        fs::write(config_path, config).unwrap();
        println!("wrote  {config_path}");
    } else {
        println!("kept   {config_path}  (already exists)");
    }

    println!();
    println!("Run tests:");
    println!("  cd examples/counter");
    println!("  npx playwright test");
}
