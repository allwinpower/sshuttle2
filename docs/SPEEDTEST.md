# Speed test on fresh servers

The [Speed test](../.github/workflows/speedtest.yml) workflow compares sshuttle and sshuttle2 on rented [LightNode](https://www.lightnode.com) VMs. It creates the VMs, measures, writes a report and deletes the VMs again. Only the repository owner can start it.

## What it measures

A client VM (2 vCPU / 4 GB) and servers of two sizes (1 vCPU / 2 GB and 2 vCPU / 4 GB): two in the client's region ("near"), and two in a second region ("far", optional). All run Ubuntu 24.04.

For each server, every mode runs `ROUNDS` times (default 3), in a different order each round. The report shows the median.

| Mode | What runs on the client VM |
|---|---|
| No tunnel | Ookla's `speedtest`, direct |
| On the server itself | `speedtest` on the server VM: the ceiling for the server's side |
| sshuttle | `sshuttle -r SERVER 0/0 --dns` (from PyPI, version pinned in the workflow) |
| sshuttle --no-latency-control | the same, with that flag |
| sshuttle2 nft | `sshuttle2 -r SERVER 0/0 --dns --method nft`, built from the commit under test |
| sshuttle2 tun | `sshuttle2 -r SERVER 0/0 --udp` |

- **Ookla** ([Speedtest CLI](https://www.speedtest.net/apps/cli)): download, upload, idle ping, and ping while downloading and while uploading. Every mode for one server uses the same Ookla server, the one nearest to the server VM, where tunneled traffic leaves.
- **CPU and memory** (`scripts/speedtest/sample.py`, from `/proc` every 0.5 s while Ookla runs):
  - server: the tunnel's server process plus its sshd processes, which do the encryption;
  - client: the tunnel client, its helpers and its ssh.
  - CPU is the average in % of one core, with the busiest second; memory is resident memory, idle and peak.

The job summary shows the tables. The artifact `speedtest-RUN_ID` holds `results.md`, `results.json` (medians and every error) and `results.jsonl` (every single measurement).

## Setup, once

1. In the LightNode console, create an API token for this alone, so it can be revoked alone.
2. Create the environment `speedtest`: the owner must approve each run, and only `main` can use it.

   ```bash
   gh api -X PUT repos/allwinpower/sshuttle2/environments/speedtest --input - <<EOF
   {"reviewers": [{"type": "User", "id": $(gh api users/allwinpower --jq .id)}],
    "prevent_self_review": false,
    "deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
   EOF
   ```

   ```bash
   gh api -X POST repos/allwinpower/sshuttle2/environments/speedtest/deployment-branch-policies -f name=main -f type=branch
   ```

3. Copy the token, then store it as a secret of that environment only:

   ```bash
   pbpaste | gh secret set LIGHTNODE_TOKEN --env speedtest -R allwinpower/sshuttle2
   ```

4. Find the region codes: Actions → Speed test → Run workflow, action `list`. It creates nothing.

## Run it

1. Actions → Speed test → Run workflow: action `bench`, `region_a`, `region_b` (empty: near servers only), `rounds`. Or:

   ```bash
   gh workflow run speedtest.yml -f action=bench -f region_a=CODE -f region_b=CODE
   ```

2. Approve it: the run page shows "Review deployments".
3. It takes about 90 minutes with two regions and 3 rounds. The log follows the test live.

**Cost:** 5 small VMs for about 1.5 hours, and about 70 GB of traffic through the client VM.

**Failed runs:** Ookla stops with "Cannot read from socket" when its connection stalls, which happens through a tunnel that queues too much data. The report says how many runs failed and in which phase (ping, download, upload). A run that failed during the upload still counts for its download. Ookla also refuses many tests in a row from one address ("Limit reached"); the test then waits 90 s and tries once more.

**Ookla's terms:** the run accepts the CLI's license and GDPR terms (`--accept-license --accept-gdpr`) on the throwaway VMs.

## How the token stays safe

The repository is public. Anyone can open a pull request, and anyone can read the workflow logs.

- The workflow starts only from "Run workflow" (`workflow_dispatch`), which needs write access. No push, pull request or schedule starts it, and no workflow in this repository uses `pull_request_target`.
- Its jobs run only when both the actor and the triggering actor are `allwinpower`, re-runs included.
- The token is a secret of the `speedtest` environment, not of the repository. A job gets it only from `main`, and only after the owner approves the run. Pull request branches and forks can never reach it.
- Within the job, only the steps that call the LightNode API (`lightnode.sh`: create, list, delete) get the token. `lightnode.sh` hands it to curl through a file descriptor, never on a command line.
- The sshuttle2 build runs in its own job without the environment: third-party actions and build scripts never run next to the token. sshuttle, Ookla's CLI and sshuttle2 run only on the VMs, which never see it.
- The job that has the token uses only GitHub's own actions, pinned to a commit, and checks out without keeping git credentials.
- Logs: each VM's root password is random, used once to install a per-run ssh key, and masked. Every VM address and the runner's address are masked as soon as they are known. `runner.sh collect` removes them from the result files too, and refuses to publish files that still hold one.

**Leftover VMs:** the last step deletes the run's VMs (named `s2bench-RUN_ID-ROLE`) even when the run fails or is cancelled, and each run first deletes leftovers named `s2bench-*` in its regions. After a hard stop, check the LightNode console.

## Files

| File | Runs on | Does |
|---|---|---|
| `scripts/speedtest/runner.sh` | GitHub runner | creates, sets up, follows and deletes the VMs |
| `scripts/speedtest/lightnode.sh` | GitHub runner | LightNode API client ([API docs](https://apidoc.lightnode.com/en/8495480m0)) |
| `scripts/speedtest/setup-vm.sh` | each VM | installs Ookla's CLI, and on the client sshuttle and nftables |
| `scripts/speedtest/run.sh` | client VM | the rounds and modes; writes `results.jsonl` |
| `scripts/speedtest/sample.py` | both | CPU and memory of the tunnel processes |
| `scripts/speedtest/report.py` | GitHub runner | `results.jsonl` → tables and JSON |

`scripts/speedtest/dry-run.sh` runs `run.sh`, the samplers and the report in Docker, with a stand-in for Ookla's CLI. CI runs it on every pull request.
