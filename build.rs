fn main() {
    // Track the version explicitly: on Windows a reused target dir can report the
    // binary fresh when the version returns to an older one, while
    // deps/agent-traces.exe still holds the newer build (Harbor
    // tools/check_version_cache.py).
    println!("cargo:rerun-if-changed=Cargo.toml");
}
