## Install

**Any Linux or macOS**, with the install script. It checks the download against `SHA256SUMS` and puts `sshuttle2` in `/usr/local/bin` (`--user` for `~/.local/bin`):

```bash
curl -fsSL https://github.com/allwinpower/sshuttle2/releases/download/v{{VERSION}}/install.sh | sh -s -- --version {{VERSION}}
```

**Debian, Ubuntu**: download the `.deb` for your CPU (`amd64` or `arm64`) below, then:

```bash
sudo apt install ./sshuttle2_{{VERSION}}_amd64.deb
```

**Fedora, RHEL, Rocky, Alma**: download the `.rpm` for your CPU (`x86_64` or `aarch64`) below, then:

```bash
sudo dnf install ./sshuttle2-{{VERSION}}-1.x86_64.rpm
```

A `.deb` or `.rpm` from here does not update itself: install the next release's file the same way.

**By hand**: download `SHA256SUMS` and the `.tar.gz` for your system, then (on macOS, `shasum -a 256` instead of `sha256sum`):

```bash
sha256sum -c --ignore-missing SHA256SUMS
```

```bash
tar -xzf sshuttle2-{{VERSION}}-linux-x86_64.tar.gz
```

```bash
sudo install -m 755 sshuttle2-{{VERSION}}-linux-x86_64/sshuttle2 /usr/local/bin/
```

The macOS binaries are not notarized. If you downloaded one with a browser, clear the quarantine flag once: `xattr -d com.apple.quarantine sshuttle2`. The install script does not need this.

Every file has a build provenance attestation: `gh attestation verify FILE --repo allwinpower/sshuttle2`.
