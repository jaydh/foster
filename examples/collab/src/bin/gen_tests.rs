use foster_core::MachineBuilder;
use serde_json::json;
use std::fs;
use std::path::Path;

fn main() {
    let machine = MachineBuilder::new("poll", "open", json!({}))
        .state("closed")
        .pass("open",   "vote",       "open")
        .pass("open",   "close_poll", "closed")
        .pass("closed", "reset",      "open")
        .build();

    let base_url  = "http://localhost:3008";
    let out_dir   = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    let spec_path = format!("{out_dir}/collab.spec.ts");
    let cfg_path  = concat!(env!("CARGO_MANIFEST_DIR"), "/playwright.config.ts");

    fs::create_dir_all(out_dir).unwrap();
    fs::write(&spec_path, foster_testgen::generate(&machine, base_url)).unwrap();
    println!("  {}", foster_testgen::summary(&machine));
    println!("wrote  {spec_path}");

    if !Path::new(cfg_path).exists() {
        fs::write(cfg_path, foster_testgen::generate_playwright_config(base_url, "collab")).unwrap();
        println!("wrote  {cfg_path}");
    } else {
        println!("kept   {cfg_path}  (already exists)");
    }

    println!();
    println!("Run tests:");
    println!("  cd examples/collab && npx playwright test");
}
