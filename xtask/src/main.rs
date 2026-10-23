//! `cargo xtask dist`: builds the static Linux servers (with cargo-zigbuild),
//! then a release client that embeds them.
//! `cargo xtask servers`: only the servers, into dist/server/.
//! `cargo xtask client TARGET`: a release client for TARGET that embeds the
//! servers already in dist/server/. The release workflow builds the servers
//! once and every client from them, so all clients of a version upload the
//! same server.
//! `cargo xtask package TARGET`: that client as a tarball in dist/, and for
//! Linux also as .deb and .rpm (with nfpm).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

const SERVER_TARGETS: [(&str, &str); 2] = [
    ("x86_64", "x86_64-unknown-linux-musl"),
    ("aarch64", "aarch64-unknown-linux-musl"),
];

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let task = args.next().unwrap_or_default();
    let target = args.next();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    match (task.as_str(), target.as_deref()) {
        ("servers", None) => servers(&root),
        ("dist", None) => {
            servers(&root)?;
            client(&root)
        }
        ("client", Some(target)) => client_for(&root, target),
        ("package", Some(target)) => package(&root, target),
        _ => bail!("usage: cargo xtask <dist|servers|client TARGET|package TARGET>"),
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

fn client_for(root: &Path, target: &str) -> Result<()> {
    // build.rs embeds whatever servers it finds, so a missing one would
    // ship a client that cannot start on that architecture.
    let dir = root.join("dist/server");
    for (arch, _) in SERVER_TARGETS {
        let path = dir.join(format!("sshuttle2-server-{arch}"));
        if !path.is_file() {
            bail!("{} is missing; run `cargo xtask servers`", path.display());
        }
    }
    // musl clients are linked statically by zig; macOS uses the system linker.
    let build = if target.ends_with("-linux-musl") {
        "zigbuild"
    } else {
        "build"
    };
    run(cargo()
        .current_dir(root)
        .env("SSHUTTLE2_SERVER_DIR", &dir)
        .args([build, "--release", "-p", "sshuttle2", "--target", target]))?;
    let bin = client_bin(root, target);
    eprintln!(
        "  {} ({} KiB)",
        bin.display(),
        std::fs::metadata(&bin)?.len() / 1024
    );
    Ok(())
}

fn client_bin(root: &Path, target: &str) -> PathBuf {
    root.join("target").join(target).join("release/sshuttle2")
}

/// Release asset names use `macos`/`linux` and `x86_64`/`arm64`.
fn platform(target: &str) -> Result<(&'static str, &'static str)> {
    let arch = match target.split('-').next() {
        Some("x86_64") => "x86_64",
        Some("aarch64") => "arm64",
        _ => bail!("no release name for the CPU of {target}"),
    };
    let os = if target.ends_with("-apple-darwin") {
        "macos"
    } else if target.ends_with("-linux-musl") {
        "linux"
    } else {
        bail!("releases ship macOS and static (musl) Linux clients, not {target}");
    };
    Ok((os, arch))
}

fn package(root: &Path, target: &str) -> Result<()> {
    let (os, arch) = platform(target)?;
    let bin = client_bin(root, target);
    if !bin.is_file() {
        bail!(
            "{} is missing; run `cargo xtask client {target}`",
            bin.display()
        );
    }
    let out = root.join("dist");
    std::fs::create_dir_all(&out)?;

    let name = format!("sshuttle2-{VERSION}-{os}-{arch}");
    let tarball = out.join(format!("{name}.tar.gz"));
    let files = [
        (bin.clone(), "sshuttle2", 0o755),
        (root.join("LICENSE"), "LICENSE", 0o644),
        (
            root.join("third_party/ipstack/LICENSE"),
            "LICENSE-ipstack",
            0o644,
        ),
        (root.join("README.md"), "README.md", 0o644),
    ];
    write_tarball(&tarball, &name, &files)?;
    eprintln!("  {}", tarball.display());

    if os == "linux" {
        let (deb_arch, rpm_arch) = match arch {
            "x86_64" => ("amd64", "x86_64"),
            _ => ("arm64", "aarch64"),
        };
        // Named here, not by nfpm: it would write a prerelease as
        // 0.1.0~rc.1, and GitHub renames assets with a `~` in them.
        for (packager, file) in [
            ("deb", format!("sshuttle2_{VERSION}_{deb_arch}.deb")),
            ("rpm", format!("sshuttle2-{VERSION}-1.{rpm_arch}.rpm")),
        ] {
            let dest = out.join(file);
            run(Command::new("nfpm")
                .current_dir(root)
                .env("VERSION", VERSION)
                .env("ARCH", deb_arch)
                .env("BINARY", &bin)
                .args(["package", "--config", "packaging/nfpm.yaml"])
                .args(["--packager", packager, "--target"])
                .arg(&dest))
            .context("is nfpm installed? (brew install nfpm)")?;
            eprintln!("  {}", dest.display());
        }
    }
    Ok(())
}

/// Writes `files` into `dir/` in a .tar.gz, owned by root, with no macOS
/// extended attributes. The time is SOURCE_DATE_EPOCH when set (the release
/// workflow sets the commit time), so rebuilding a tag gives the same files.
fn write_tarball(path: &Path, dir: &str, files: &[(PathBuf, &str, u32)]) -> Result<()> {
    let mtime = match std::env::var("SOURCE_DATE_EPOCH") {
        Ok(s) => s.parse().context("SOURCE_DATE_EPOCH")?,
        Err(_) => SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    };
    let gz = flate2::write::GzEncoder::new(File::create(path)?, flate2::Compression::best());
    let mut tar = tar::Builder::new(gz);
    for (src, name, mode) in files {
        let data = std::fs::read(src).with_context(|| format!("reading {}", src.display()))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(*mode);
        header.set_mtime(mtime);
        header.set_uid(0);
        header.set_gid(0);
        tar.append_data(&mut header, format!("{dir}/{name}"), data.as_slice())?;
    }
    tar.into_inner()?.finish()?;
    Ok(())
}
