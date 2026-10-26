//! Getting the server running on the remote host over the one ssh session.
//!
//! The remote command is a short POSIX sh script. It reports the machine's
//! OS and architecture, looks for a cached server binary matching the hash the
//! client sends, and asks for an upload only on a miss. Each step waits for
//! the other side, never pipelining: `head -c` may read past its count on a
//! pipe, so the client sends nothing the script has not asked for.
//!
//! Every line the script prints starts with a per-run nonce, so whatever the
//! login shell prints first (MOTD, rc files) is skipped.

use std::borrow::Cow;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use bytes::BytesMut;
use sha2::{Digest, Sha256};
use sshuttle2_core::handshake::read_tagged_line;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tracing::{debug, info};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
}

/// The script, one line long: csh rejects newlines inside quotes, so the
/// login shell must see a single line. No single quotes (it is wrapped in
/// them) and no `!` (csh history expansion).
const SCRIPT: &str = r#"
N=__NONCE__;
U=$(id -u);
say() { printf "%s %s\n" "$N" "$*"; };
A=$(uname -m);
case "$A" in amd64) A=x86_64 ;; arm64|armv8*) A=aarch64 ;; esac;
say PLATFORM "$(uname -s)" "$A";
read H S || exit 1;
F="sshuttle2-server-$H";
set -- "${XDG_CACHE_HOME:-$HOME/.cache}/sshuttle2" "/tmp/sshuttle2-$U" "/dev/shm/sshuttle2-$U";
[ -n "$XDG_RUNTIME_DIR" ] && set -- "$1" "$XDG_RUNTIME_DIR/sshuttle2" "$2" "$3";
ok() {
  case "$1" in /tmp/*|/dev/shm/*) [ -L "$1" ] && return 1 ;; esac;
  mkdir -p "$1" 2>/dev/null || return 1;
  chmod 700 "$1" 2>/dev/null || return 1;
  [ -w "$1" ] || return 1;
  case "$1" in /tmp/*|/dev/shm/*) [ "$(stat -c %u "$1" 2>/dev/null || stat -f %u "$1" 2>/dev/null)" = "$U" ] || return 1 ;; esac;
};
B=;
for D in "$@"; do
  [ -d "$D" ] || continue; ok "$D" || continue;
  if [ -x "$D/$F" ] && "$D/$F" --self-check "$H" >/dev/null 2>&1; then B="$D/$F"; break; fi;
done;
T=;
if [ -z "$B" ]; then
  for D in "$@"; do
    ok "$D" || continue;
    if [ -z "$T" ]; then
      T="$D/.tmp.$$"; say NEED;
      head -c "$S" >"$T" || { rm -f "$T"; say ERR "upload to $D failed"; exit 1; };
    else
      cp "$T" "$D/.tmp.$$" 2>/dev/null || continue; rm -f "$T"; T="$D/.tmp.$$";
    fi;
    chmod 700 "$T";
    "$T" --self-check "$H" >/dev/null 2>&1; R=$?;
    if [ "$R" = 0 ]; then mv -f "$T" "$D/$F" && B="$D/$F"; break; fi;
    case "$R" in 126) ;; *) rm -f "$T"; say ERR "uploaded server failed its self-check (exit $R)"; exit 1 ;; esac;
  done;
fi;
[ -n "$T" ] && [ -z "$B" ] && rm -f "$T";
[ -n "$B" ] || { say ERR "found no directory where the server can run (noexec mounts?)"; exit 1; };
for f in "${B%/*}"/sshuttle2-server-*; do [ "$f" = "$B" ] || find "$f" -mtime +7 -exec rm -f {} + 2>/dev/null; done;
touch "$B" 2>/dev/null;
say GO "$B";
exec "$B" server --nonce "$N" __ARGS__
"#;

/// The bootstrap script for one run, collapsed to a single line.
pub fn script(nonce: &str, server_args: &str) -> String {
    let s = SCRIPT
        .replace("__NONCE__", nonce)
        .replace("__ARGS__", server_args);
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    debug_assert!(!one_line.contains('\'') && !one_line.contains('!'));
    one_line
}

/// The remote command handed to ssh.
pub fn remote_command(nonce: &str, server_args: &str) -> String {
    format!("/bin/sh -c '{}'", script(nonce, server_args))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Platforms (`linux-x86_64`, `macos-aarch64`, ...) with a server binary
/// built into this client.
pub fn embedded_platforms() -> Vec<&'static str> {
    embedded::SERVERS.iter().map(|(a, _)| *a).collect()
}

/// The embedded-server name for the script's `PLATFORM` line: `uname -s`,
/// then the architecture the script has already normalized. Other systems
/// keep their own name, so the error lists what this build has, and
/// `--server-bin` can still bring a server for them.
fn platform(line: &str) -> Result<String> {
    let (os, arch) = line
        .trim()
        .split_once(' ')
        .with_context(|| format!("unexpected platform from remote: {line:?}"))?;
    let os = match os {
        "Linux" => "linux".to_string(),
        "Darwin" => "macos".to_string(),
        other => other.to_lowercase(),
    };
    Ok(format!("{os}-{arch}"))
}

fn server_binary<'a>(platform: &str, override_path: Option<&Path>) -> Result<Cow<'a, [u8]>> {
    if let Some(p) = override_path {
        return Ok(Cow::Owned(
            std::fs::read(p).with_context(|| format!("reading {}", p.display()))?,
        ));
    }
    embedded::SERVERS
        .iter()
        .find(|(p, _)| *p == platform)
        .map(|(_, b)| Cow::Borrowed(*b))
        .ok_or_else(|| {
            let have = embedded_platforms();
            if have.is_empty() {
                anyhow!(
                    "this build carries no server binaries; build with `cargo xtask dist` \
                     or pass --server-bin PATH (a build of sshuttle2-server for the remote host)"
                )
            } else {
                anyhow!(
                    "remote host is {platform}; this build has servers for {}",
                    have.join(", ")
                )
            }
        })
}

/// Runs the client side of the bootstrap: answers the script's questions and
/// uploads the server if it asks. Returns once the script has exec'd it,
/// saying whether an upload happened.
pub async fn run<R, W>(
    rd: &mut R,
    wr: &mut W,
    buf: &mut BytesMut,
    nonce: &str,
    override_path: Option<&Path>,
) -> Result<bool>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let line = read_tagged_line(rd, buf, nonce).await?;
    let platform = platform(
        line.strip_prefix("PLATFORM ")
            .with_context(|| format!("unexpected line from remote: {line:?}"))?,
    )?;
    let binary = server_binary(&platform, override_path)?;
    let hash = sha256_hex(&binary);
    debug!(%platform, %hash, "remote platform");
    wr.write_all(format!("{hash} {}\n", binary.len()).as_bytes())
        .await?;
    wr.flush().await?;
    let mut uploaded = false;
    loop {
        let line = read_tagged_line(rd, buf, nonce).await?;
        if line == "NEED" {
            info!(
                "uploading server to the remote host ({} KiB, once per version)",
                binary.len() / 1024
            );
            wr.write_all(&binary).await?;
            wr.flush().await?;
            uploaded = true;
        } else if let Some(path) = line.strip_prefix("GO ") {
            debug!("remote server at {path}");
            return Ok(uploaded);
        } else if let Some(msg) = line.strip_prefix("ERR ") {
            bail!("remote: {msg}");
        } else {
            bail!("unexpected line from remote: {line:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_names() {
        assert_eq!(platform("Linux x86_64").unwrap(), "linux-x86_64");
        assert_eq!(platform("Darwin aarch64\n").unwrap(), "macos-aarch64");
        assert_eq!(platform("FreeBSD x86_64").unwrap(), "freebsd-x86_64");
        assert!(platform("Linux").is_err());
    }

    #[test]
    fn script_is_one_safe_line() {
        let s = remote_command("abc123", "-v");
        assert!(!s.contains('\n'));
        assert_eq!(s.matches('\'').count(), 2, "only the wrapping quotes");
        assert!(!s.contains('!'));
        assert!(s.contains("N=abc123;"));
        assert!(s.ends_with("server --nonce \"$N\" -v'"));
    }

    /// Stands in for the server: passes any self-check, prints the banner.
    const FAKE_SERVER: &str = "#!/bin/sh\ncase \"$1\" in --self-check) exit 0 ;; server) printf '%s SSHUTTLE2 1\\n' \"$3\" ;; esac\n";

    /// Runs the real remote command through `shell -c`, as sshd would.
    async fn bootstrap_once(shell: &str, cache: &Path, fake: &Path) -> Result<bool> {
        use std::process::Stdio;
        let nonce = "tstnonce";
        let mut child = tokio::process::Command::new(shell)
            .arg("-c")
            .arg(remote_command(nonce, ""))
            .env("XDG_CACHE_HOME", cache)
            .env_remove("XDG_RUNTIME_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut rd = child.stdout.take().unwrap();
        let mut wr = child.stdin.take().unwrap();
        let mut buf = BytesMut::new();
        let uploaded = run(&mut rd, &mut wr, &mut buf, nonce, Some(fake)).await?;
        sshuttle2_core::handshake::read_banner(&mut rd, &mut buf, nonce).await?;
        Ok(uploaded)
    }

    #[tokio::test]
    async fn uploads_once_then_uses_cache() {
        let dir = std::env::temp_dir().join(format!("tsbt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("fake-server");
        std::fs::write(&fake, FAKE_SERVER).unwrap();
        let cache = dir.join("cache");
        let hash = sha256_hex(FAKE_SERVER.as_bytes());

        assert!(
            bootstrap_once("sh", &cache, &fake).await.unwrap(),
            "first run uploads"
        );
        let cached = cache
            .join("sshuttle2")
            .join(format!("sshuttle2-server-{hash}"));
        assert_eq!(std::fs::read(&cached).unwrap(), FAKE_SERVER.as_bytes());
        assert!(
            !bootstrap_once("sh", &cache, &fake).await.unwrap(),
            "second run hits cache"
        );

        // A csh login shell must accept the command line too.
        if Path::new("/bin/csh").exists() {
            assert!(!bootstrap_once("/bin/csh", &cache, &fake).await.unwrap());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
