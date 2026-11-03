//! Lets `WEBPRINT_DEFAULT_SERVER` be baked into the binary at build time.
//!
//! Without this, cargo would not notice the variable changing and could hand
//! back a stale binary with the previous address compiled in.
fn main() {
    println!("cargo:rerun-if-env-changed=WEBPRINT_DEFAULT_SERVER");
}
