//! Embeds the Linux server binaries, when they have been built, so the client
//! can upload the right one. `cargo xtask dist` builds them first; a plain
//! `cargo build` embeds nothing and the client needs `--server-bin`.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=SSHUTTLE2_SERVER_DIR");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dir = std::env::var_os("SSHUTTLE2_SERVER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../dist/server"));
    let mut out = String::from("pub static SERVERS: &[(&str, &[u8])] = &[\n");
    for arch in ["x86_64", "aarch64"] {
        let path = dir.join(format!("sshuttle2-server-{arch}"));
        println!("cargo:rerun-if-changed={}", path.display());
        if let Ok(path) = path.canonicalize() {
            out.push_str(&format!("    ({arch:?}, include_bytes!({path:?})),\n"));
        }
    }
    out.push_str("];\n");
    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("embedded.rs");
    std::fs::write(dest, out).unwrap();
}
