//! Print every registered tool and its JSON Schema.
//!
//! Run with: `cargo run -p tracker-mcp --example list_tools`

fn main() {
    for def in tracker_mcp::tool_defs() {
        println!("# {} — {}", def.name, def.description);
        println!(
            "{}\n",
            serde_json::to_string_pretty(&def.input_schema).unwrap()
        );
    }
}
