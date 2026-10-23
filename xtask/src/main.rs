//! `cargo xtask dist`: builds the static Linux servers (with cargo-zigbuild),
//! then a release client that embeds them.
//! `cargo xtask servers`: only the servers, into dist/server/.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

const SERVER_TARGETS: [(&str, &str); 2] = [
    ("x86_64", "x86_64-unknown-linux-musl"),
    ("aarch64", "aarch64-unknown-linux-musl"),
];

fn main() -> Result<()> {
    let task = std::env::args().nth(1).unwrap_or_default();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    match task.as_str() {
        "servers" => servers(&root),
        "dist" => {
            servers(&root)?;
            client(&root)
        }
        _ => bail!("usage: cargo xtask <dist|servers>"),
    }
}

fn cargo() -> Command {
    Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
}

fn run(cmd: &mut Command) -> Result<()> {
    eprintln!("+ {cmd:?}");
    let st = cmd.status().with_context(|| format!("running {cmd:?}"))?;
    if !st.success() {
        bail!("{cmd:?} failed: {st}");
    }
    Ok(())
}

fn servers(root: &Path) -> Result<()> {
    let out = root.join("dist/server");
    std::fs::create_dir_all(&out)?;
    for (arch, target) in SERVER_TARGETS {
        run(cargo()
            .current_dir(root)
            .args([
                "zigbuild",
                "--profile",
                "release-server",
                "-p",
                "sshuttle2-server",
            ])
            .args(["--target", target]))
        .context("is cargo-zigbuild installed? (brew install zig cargo-zigbuild)")?;
        let built: PathBuf = root
            .join("target")
            .join(target)
            .join("release-server/sshuttle2-server");
        let dest = out.join(format!("sshuttle2-server-{arch}"));
        std::fs::copy(&built, &dest)
            .with_context(|| format!("copying {} to {}", built.display(), dest.display()))?;
        eprintln!(
            "  {} ({} KiB)",
            dest.display(),
            std::fs::metadata(&dest)?.len() / 1024
        );
    }
    Ok(())
}

fn client(root: &Path) -> Result<()> {
    run(cargo()
        .current_dir(root)
        .env("SSHUTTLE2_SERVER_DIR", root.join("dist/server"))
        // The native server too: `-r local` (tests) runs it from target/release.
        .args([
            "build",
            "--release",
            "-p",
            "sshuttle2",
            "-p",
            "sshuttle2-server",
        ]))?;
    let bin = root.join("target/release/sshuttle2");
    eprintln!(
        "  {} ({} KiB)",
        bin.display(),
        std::fs::metadata(&bin)?.len() / 1024
    );
    Ok(())
}
