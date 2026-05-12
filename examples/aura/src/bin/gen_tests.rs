use foster_core::{MachineBuilder, MachineError};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

fn stub(c: Value, _: Value) -> Result<Value, MachineError> { Ok(c) }

fn main() {
    let machine = MachineBuilder::new("aura", "calm", json!({}))
        .state("focused")
        .state("energized")
        .state("overwhelmed")
        .on("calm",       "focus",     "focused",    Some(stub))
        .on("calm",       "energize",  "energized",  Some(stub))
        .on("calm",       "overwhelm", "overwhelmed",Some(stub))
        .on("focused",    "calm",      "calm",        Some(stub))
        .on("focused",    "energize",  "energized",  Some(stub))
        .on("focused",    "overwhelm", "overwhelmed",Some(stub))
        .on("energized",  "calm",      "calm",        Some(stub))
        .on("energized",  "focus",     "focused",    Some(stub))
        .on("energized",  "overwhelm", "overwhelmed",Some(stub))
        .on("overwhelmed","calm",      "calm",        Some(stub))
        .on("overwhelmed","focus",     "focused",    Some(stub))
        .on("overwhelmed","energize",  "energized",  Some(stub))
        .build();

    let base_url  = "http://localhost:3003";
    let out_dir   = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    let spec_path = format!("{out_dir}/aura.spec.ts");
    let cfg_path  = concat!(env!("CARGO_MANIFEST_DIR"), "/playwright.config.ts");

    fs::create_dir_all(out_dir).unwrap();
    fs::write(&spec_path, foster_testgen::generate(&machine, base_url)).unwrap();
    println!("wrote  {spec_path}");

    if !Path::new(cfg_path).exists() {
        fs::write(cfg_path, foster_testgen::generate_playwright_config(base_url)).unwrap();
        println!("wrote  {cfg_path}");
    }
}
