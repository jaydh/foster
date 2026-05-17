use foster_core::MachineBuilder;
use serde_json::json;
use std::fs;
use std::path::Path;

fn main() {
    let machine = MachineBuilder::new("aura", "calm", json!({}))
        .state("focused")
        .state("energized")
        .state("overwhelmed")
        .pass("calm",       "focus",     "focused")
        .pass("calm",       "energize",  "energized")
        .pass("calm",       "overwhelm", "overwhelmed")
        .pass("focused",    "calm",      "calm")
        .pass("focused",    "energize",  "energized")
        .pass("focused",    "overwhelm", "overwhelmed")
        .pass("energized",  "calm",      "calm")
        .pass("energized",  "focus",     "focused")
        .pass("energized",  "overwhelm", "overwhelmed")
        .pass("overwhelmed","calm",      "calm")
        .pass("overwhelmed","focus",     "focused")
        .pass("overwhelmed","energize",  "energized")
        .build();

    let base_url  = "http://localhost:3003";
    let out_dir   = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    let spec_path = format!("{out_dir}/aura.spec.ts");
    let cfg_path  = concat!(env!("CARGO_MANIFEST_DIR"), "/playwright.config.ts");

    let sdk_path = format!("{out_dir}/aura.sdk.ts");

    fs::create_dir_all(out_dir).unwrap();
    fs::write(&spec_path, foster_testgen::generate(&machine, base_url)).unwrap();
    println!("  {}", foster_testgen::summary(&machine));
    println!("wrote  {spec_path}");

    fs::write(&sdk_path, foster_testgen::generate_sdk(&machine, base_url)).unwrap();
    println!("wrote  {sdk_path}");

    if !Path::new(cfg_path).exists() {
        fs::write(cfg_path, foster_testgen::generate_playwright_config(base_url, "aura")).unwrap();
        println!("wrote  {cfg_path}");
    }
}
