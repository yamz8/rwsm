//! Generates the Unicode tables (`src/unicode_tables.rs`) with the Python
//! uwsm runs on, so `repr()` escapes and `float()` digits match its Unicode
//! version: `RWSM_PYTHON`, else `/usr/bin/python3` (what uwsm's shebang
//! names; a `python3` earlier in PATH, such as a venv, is not used). Without
//! a working interpreter the committed tables are used.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo::rerun-if-changed=tools/gen-unicode-tables.py");
    println!("cargo::rerun-if-changed=src/unicode_tables.rs");
    println!("cargo::rerun-if-env-changed=RWSM_PYTHON");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("unicode_tables.rs");
    let python = std::env::var("RWSM_PYTHON").unwrap_or_else(|_| "/usr/bin/python3".into());
    let generated = Command::new(&python)
        .args(["-I", "tools/gen-unicode-tables.py"])
        .output()
        .ok()
        .filter(|o| o.status.success() && !o.stdout.is_empty());
    match generated {
        Some(o) => std::fs::write(&out, o.stdout).unwrap(),
        None => {
            println!("cargo::warning=could not run {python}; using the committed Unicode tables");
            std::fs::copy("src/unicode_tables.rs", &out).unwrap();
        }
    }
}
