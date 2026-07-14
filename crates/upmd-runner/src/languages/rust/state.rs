// State capture functionality for Rust
// This code is injected into user code for experimental state capture

use std::env;
use std::fs::File;
use std::io::Write;

fn upmd_capture_state() {
    upmd_write_state();
}

fn upmd_state_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn upmd_write_state() {
    let env_content: String = env::vars()
        .map(|(key, value)| {
            format!(
                "env \"{}\" \"{}\"",
                upmd_state_escape(&key),
                upmd_state_escape(&value)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let cwd = env::current_dir()
        .map(|cwd| cwd.to_string_lossy().to_string())
        .unwrap_or_default();
    let state = format!("version 1\ncwd \"{}\"\n{}\n", upmd_state_escape(&cwd), env_content);

    if let Ok(state_fifo) = env::var("UPMD_STATE_FIFO") {
        if let Ok(mut file) = File::create(&state_fifo) {
            let _ = file.write_all(state.as_bytes());
        }
    }
}
