use foster_core::{MachineBuilder, MachineError};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

// Minimal reducer stubs — gen_tests only needs the machine graph, not full logic.
fn stub(c: Value, _: Value) -> Result<Value, MachineError> { Ok(c) }

fn main() {
    let machine = MachineBuilder::new(
        "kanban",
        "viewing",
        json!({
            "tasks": [
                { "id": "1", "title": "Design state model", "column": "done" },
                { "id": "2", "title": "Build WASM client",  "column": "in_progress" },
                { "id": "3", "title": "Write tests",         "column": "todo" }
            ],
            "draft_title": "",
            "editing_id":  "",
            "confirm_id":  ""
        }),
    )
    .state("creating")
    .state("editing")
    .state("confirming_delete")
    .on("viewing",           "start_create", "creating",          Some(stub))
    .on("viewing",           "start_edit",   "editing",           Some(stub))
    .on("viewing",           "start_delete", "confirming_delete", Some(stub))
    .on("viewing",           "move_task",    "viewing",           Some(stub))
    .on("creating",          "save",         "viewing",           Some(stub))
    .on("creating",          "cancel",       "viewing",           Some(stub))
    .on("editing",           "save",         "viewing",           Some(stub))
    .on("editing",           "cancel",       "viewing",           Some(stub))
    .on("confirming_delete", "confirm",      "viewing",           Some(stub))
    .on("confirming_delete", "cancel",       "viewing",           Some(stub))
    .build();

    let base_url  = "http://localhost:3002";
    let out_dir   = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    let spec_path = format!("{out_dir}/kanban.spec.ts");
    let cfg_path  = concat!(env!("CARGO_MANIFEST_DIR"), "/playwright.config.ts");

    fs::create_dir_all(out_dir).unwrap();
    fs::write(&spec_path, foster_testgen::generate(&machine, base_url)).unwrap();
    println!("wrote  {spec_path}");

    if !Path::new(cfg_path).exists() {
        fs::write(cfg_path, foster_testgen::generate_playwright_config(base_url)).unwrap();
        println!("wrote  {cfg_path}");
    }
}
