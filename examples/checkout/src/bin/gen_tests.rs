use foster_core::MachineBuilder;
use serde_json::json;
use std::fs;
use std::path::Path;

fn main() {
    let machine = MachineBuilder::new("checkout", "cart", json!({}))
        .state("shipping")
        .state("payment")
        .state("review")
        .state("processing")
        .state("confirmed")
        .state("failed")
        .pass("cart",       "start_checkout", "shipping")
        .pass("shipping",   "save_shipping",  "payment")
        .pass("shipping",   "back",           "cart")
        .pass("payment",    "save_payment",   "review")
        .pass("payment",    "back",           "shipping")
        .pass("review",     "place_order",    "processing")
        .pass("review",     "back",           "payment")
        .pass("processing", "succeed",        "confirmed")
        .pass("processing", "fail",           "failed")
        .pass("failed",     "retry",          "payment")
        .pass("confirmed",  "new_order",      "cart")
        .build();

    let base_url  = "http://localhost:3004";
    let out_dir   = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    let spec_path = format!("{out_dir}/checkout.spec.ts");
    let cfg_path  = concat!(env!("CARGO_MANIFEST_DIR"), "/playwright.config.ts");

    let sdk_path = format!("{out_dir}/checkout.sdk.ts");

    fs::create_dir_all(out_dir).unwrap();
    fs::write(&spec_path, foster_testgen::generate(&machine, base_url)).unwrap();
    println!("  {}", foster_testgen::summary(&machine));
    println!("wrote  {spec_path}");

    fs::write(&sdk_path, foster_testgen::generate_sdk(&machine, base_url)).unwrap();
    println!("wrote  {sdk_path}");

    if !Path::new(cfg_path).exists() {
        fs::write(cfg_path, foster_testgen::generate_playwright_config(base_url, "checkout")).unwrap();
        println!("wrote  {cfg_path}");
    } else {
        println!("kept   {cfg_path}  (already exists)");
    }

    println!();
    println!("Run tests:");
    println!("  cd examples/checkout && npx playwright test");
}
