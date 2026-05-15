use foster_core::MachineBuilder;
use serde_json::json;
use std::fs;
use std::path::Path;

fn main() {
    let machine = MachineBuilder::new("notion", "reading", json!({}))
        .state("editing")
        .pass("reading", "focus_block",  "editing")
        .pass("reading", "add_block",    "editing")
        .pass("reading", "delete_block", "reading")
        .pass("reading", "move_up",      "reading")
        .pass("reading", "move_down",    "reading")
        .pass("reading", "toggle_todo",  "reading")
        .pass("reading", "update_title", "reading")
        .pass("editing", "commit_edit",  "reading")
        .pass("editing", "discard_edit", "reading")
        .pass("editing", "change_type",  "editing")
        .build();

    let base_url  = "http://localhost:3006";
    let out_dir   = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    let spec_path = format!("{out_dir}/notion.spec.ts");
    let cfg_path  = concat!(env!("CARGO_MANIFEST_DIR"), "/playwright.config.ts");

    fs::create_dir_all(out_dir).unwrap();
    fs::write(&spec_path, foster_testgen::generate(&machine, base_url)).unwrap();
    println!("wrote  {spec_path}");

    if !Path::new(cfg_path).exists() {
        fs::write(cfg_path, foster_testgen::generate_playwright_config(base_url, "notion")).unwrap();
        println!("wrote  {cfg_path}");
    } else {
        println!("kept   {cfg_path}  (already exists)");
    }

    println!();
    println!("Run tests:");
    println!("  cd examples/notion && npx playwright test");
}
