# Security policy

## Supported versions

Only the [latest release](https://github.com/allwinpower/sshuttle2/releases/latest) gets security fixes. Older releases do not.

## Reporting a vulnerability

Report security problems privately, not in a public issue:

1. Open the repository's [Security tab](https://github.com/allwinpower/sshuttle2/security).
2. Click "Report a vulnerability" ([direct link](https://github.com/allwinpower/sshuttle2/security/advisories/new)).

Include:

- the sshuttle2 version (`sshuttle2 --version`)
- the client and server operating systems
- the method (tun, pf, nft or SOCKS5) and the exact command
- steps to reproduce, and what an attacker gains

The report stays private between you and the maintainers until a fixed release is out. The advisory is published with that release and credits you, unless you ask not to be named.

## Scope

Examples of what counts:

- Another local user or process using the privileged helper, which runs as root through sudo.
- Traffic that should go through the tunnel going around it.
- The server side making the client do more than carry traffic.
- Another user on the server replacing the cached server binary in `~/.cache/sshuttle2/`.

Problems in OpenSSH itself go to the [OpenSSH project](https://www.openssh.com/security.html).
