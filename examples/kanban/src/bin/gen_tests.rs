use foster_core::MachineBuilder;
use serde_json::json;
use std::fs;
use std::path::Path;

fn main() {
    let machine = MachineBuilder::new("kanban", "viewing", json!({
        "tasks": [], "draft_title": "", "editing_id": "", "confirm_id": ""
    }))
    .state("creating")
    .state("editing")
    .state("confirming_delete")
    .pass("viewing",           "start_create", "creating")
    .pass("viewing",           "start_edit",   "editing")
    .pass("viewing",           "start_delete", "confirming_delete")
    .pass("viewing",           "move_task",    "viewing")
    .pass("creating",          "save",         "viewing")
    .pass("creating",          "cancel",       "viewing")
    .pass("editing",           "save",         "viewing")
    .pass("editing",           "cancel",       "viewing")
    .pass("confirming_delete", "confirm",      "viewing")
    .pass("confirming_delete", "cancel",       "viewing")
    .build();

    let base_url  = "http://localhost:3002";
    let out_dir   = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    let spec_path = format!("{out_dir}/kanban.spec.ts");
    let cfg_path  = concat!(env!("CARGO_MANIFEST_DIR"), "/playwright.config.ts");

    fs::create_dir_all(out_dir).unwrap();
    fs::write(&spec_path, foster_testgen::generate(&machine, base_url)).unwrap();
    println!("wrote  {spec_path}");

    if !Path::new(cfg_path).exists() {
        fs::write(cfg_path, foster_testgen::generate_playwright_config(base_url, "kanban")).unwrap();
        println!("wrote  {cfg_path}");
    }
}
