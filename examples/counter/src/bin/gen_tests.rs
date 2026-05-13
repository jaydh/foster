use foster_core::MachineBuilder;
use serde_json::json;
use std::fs;
use std::path::Path;

fn main() {
    let machine = MachineBuilder::new("counter", "idle", json!({ "count": 0 }))
        .state("error")
        .pass("idle",  "increment", "idle")
        .pass("idle",  "decrement", "idle")
        .pass("idle",  "reset",     "idle")
        .pass("idle",  "break_it",  "error")
        .pass("error", "recover",   "idle")
        .build();

    let base_url  = "http://localhost:3000";
    let out_dir   = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    let spec_path = format!("{out_dir}/counter.spec.ts");
    let cfg_path  = concat!(env!("CARGO_MANIFEST_DIR"), "/playwright.config.ts");

    let sdk_path = format!("{out_dir}/counter.sdk.ts");

    fs::create_dir_all(out_dir).unwrap();
    fs::write(&spec_path, foster_testgen::generate(&machine, base_url)).unwrap();
    println!("wrote  {spec_path}");

    fs::write(&sdk_path, foster_testgen::generate_sdk(&machine, base_url)).unwrap();
    println!("wrote  {sdk_path}");

    if !Path::new(cfg_path).exists() {
        fs::write(cfg_path, foster_testgen::generate_playwright_config(base_url, "counter")).unwrap();
        println!("wrote  {cfg_path}");
    } else {
        println!("kept   {cfg_path}  (already exists)");
    }

    println!();
    println!("Run tests:");
    println!("  cd examples/counter && npx playwright test");
}
