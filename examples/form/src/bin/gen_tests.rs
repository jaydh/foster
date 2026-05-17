use foster_core::MachineBuilder;
use serde_json::json;
use std::fs;
use std::path::Path;

fn main() {
    let machine = MachineBuilder::new("form", "step1", json!({}))
        .state("step2")
        .state("step3")
        .state("review")
        .state("done")
        .pass("step1",  "validate1", "step1")
        .pass("step1",  "advance1",  "step2")
        .pass("step2",  "validate2", "step2")
        .pass("step2",  "advance2",  "step3")
        .pass("step2",  "back1",     "step1")
        .pass("step3",  "validate3", "step3")
        .pass("step3",  "advance3",  "review")
        .pass("step3",  "back2",     "step2")
        .pass("review", "submit",    "done")
        .pass("review", "back3",     "step3")
        .pass("done",   "reset",     "step1")
        .build();

    let base_url  = "http://localhost:3007";
    let out_dir   = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    let spec_path = format!("{out_dir}/form.spec.ts");
    let cfg_path  = concat!(env!("CARGO_MANIFEST_DIR"), "/playwright.config.ts");

    fs::create_dir_all(out_dir).unwrap();
    fs::write(&spec_path, foster_testgen::generate(&machine, base_url)).unwrap();
    println!("  {}", foster_testgen::summary(&machine));
    println!("wrote  {spec_path}");

    if !Path::new(cfg_path).exists() {
        fs::write(cfg_path, foster_testgen::generate_playwright_config(base_url, "form")).unwrap();
        println!("wrote  {cfg_path}");
    } else {
        println!("kept   {cfg_path}  (already exists)");
    }

    println!();
    println!("Run tests:");
    println!("  cd examples/form && npx playwright test");
}
