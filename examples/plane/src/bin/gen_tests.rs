use foster_core::MachineBuilder;
use serde_json::json;
use std::fs;
use std::path::Path;

fn main() {
    let machine = MachineBuilder::new("plane", "list", json!({}))
        .state("filter")
        .state("detail")
        .state("create")
        .state("edit")
        .pass("list",   "open_create",   "create")
        .pass("list",   "open_issue",    "detail")
        .pass("list",   "toggle_filter", "filter")
        .pass("filter", "open_issue",    "detail")
        .pass("filter", "apply_filter",  "list")
        .pass("filter", "clear_filter",  "list")
        .pass("filter", "close_filter",  "list")
        .pass("detail", "back",          "list")
        .pass("detail", "start_edit",    "edit")
        .pass("detail", "add_comment",   "detail")
        .pass("detail", "delete_issue",  "list")
        .pass("create", "save_create",   "list")
        .pass("create", "cancel_create", "list")
        .pass("edit",   "save_edit",     "detail")
        .pass("edit",   "cancel_edit",   "detail")
        .build();

    let base_url  = "http://localhost:3005";
    let out_dir   = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    let spec_path = format!("{out_dir}/plane.spec.ts");
    let cfg_path  = concat!(env!("CARGO_MANIFEST_DIR"), "/playwright.config.ts");

    fs::create_dir_all(out_dir).unwrap();
    fs::write(&spec_path, foster_testgen::generate(&machine, base_url)).unwrap();
    println!("  {}", foster_testgen::summary(&machine));
    println!("wrote  {spec_path}");

    if !Path::new(cfg_path).exists() {
        fs::write(cfg_path, foster_testgen::generate_playwright_config(base_url, "plane")).unwrap();
        println!("wrote  {cfg_path}");
    } else {
        println!("kept   {cfg_path}  (already exists)");
    }

    println!();
    println!("Run tests:");
    println!("  cd examples/plane && npx playwright test");
}
