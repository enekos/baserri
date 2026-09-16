# baserri

Provisioner and Telegram control plane for a home Raspberry Pi.

> A *baserri* is the Basque farmstead: house, workshop and stores under one
> roof, built to need nobody. That is the idea — your own forge, your own CI,
> your own services, on hardware you can touch.

Two binaries, one crate, **zero dependencies**:

- **`baserri`** runs on your laptop. It probes the box over ssh, tells you what is not
  yet true, and converges it one idempotent step at a time.
- **`baserrid`** runs on the Pi. It long-polls Telegram, answers `/status`,
  runs named jobs, runs arbitrary shell after an explicit confirm, and pushes
  an alert when the disk fills, the SoC throttles or a unit dies.

## Why no dependencies

`aarch64-unknown-linux-musl` cross-compiles from macOS with `rust-lld` and no C
toolchain — as long as nothing in the tree is C. Any TLS crate (ring, aws-lc,
openssl) breaks that and drags in a cross-gcc. So the network calls shell out to
`curl` and the remote calls shell out to `ssh`/`scp`. The result is one static
ELF you `scp` and run — no runtime, no agent to install first.

```
cargo build --release --target aarch64-unknown-linux-musl --bin baserrid
```

## Quickstart

```bash
baserri init            # writes baserri.conf + baserrid.conf, both gitignored
$EDITOR baserri.conf    # host, hostname, timezone, lan_cidr
$EDITOR baserrid.conf   # bot token + your chat id (ask @userinfobot)

baserri doctor          # what the box is, and what will bite
baserri plan            # what apply would change, nothing written
baserri apply           # converge
baserri ship            # rebuild baserrid and push just that
baserri logs 100
```

## The step model

Every step is a pair of shell scripts sent to the box over one multiplexed ssh
connection, as a script on **stdin** — never as arguments, because ssh
re-splits a remote command through a second shell and quotes do not survive.

A check exits `0` when the step is already satisfied, `10` when it is not, and
anything else when the check itself broke. That third case is the point: a
failed check reports `blocked`, never `todo`, so baserri cannot "fix" something it
could not measure.

`apply` re-runs the check after applying and fails loudly if the step did not
take, so a silently ineffective step cannot pass as done.

Steps read the box before they are built: an SD-card root replaces the swapfile
step with a refusal to add disk swap to a card that write load will kill.

## The Telegram control plane

**Long-poll, never a webhook.** The Pi opens outbound connections only, so it
needs no port forward, no dynamic DNS, and no exception to the Tailscale-only
rule. It works behind CGNAT.

The security model, in the order it matters:

1. **An allowlist of chat ids.** Anything else gets no reply at all — not an
   error, not a refusal. A wrong-number probe learns nothing.
2. **`baserrid` does not run as root.** `/sh` runs as the `baserri` system user. A
   narrow sudoers file grants `systemctl`, `reboot` and `docker`, and nothing
   else. There is no path from a Telegram message to unrestricted root.
3. **`/sh` and `/reboot` need a confirm.** The command is echoed back with a
   random token that expires in 90 seconds and is bound to the chat that armed
   it. A token from another allowed chat does not land.
4. **Every command is bounded.** `timeout` kills it, `ulimit -f` caps what it
   can write, and the reply is clipped on a character boundary.
5. **A restart never replays.** On boot the daemon drains the backlog without
   executing it, so a queued `/yes` from before a crash does nothing.

`/sh` can be turned off entirely with `allow_shell = false`.

### Commands

```
/status          load, memory, temperature, throttle flags, disk, containers
/df              filesystem usage
/jobs            the named jobs in baserrid.conf
/run <job>       run one of them
/logs <unit> [n] journal tail, unit names validated against a charset
/sh <cmd>        anything, after a confirm
/reboot          after a confirm
/no              drop whatever is armed
```

### Alerts

A background thread re-probes on an interval and messages you on the
**transition**, not every tick: disk over a threshold, SoC over a temperature,
non-zero throttle flags, a named unit that stopped. Recovery is reported once
too.

## Config

`baserri.conf` (the laptop side) and `baserrid.conf` (the box side) share one parser:
`key = value`, `[section]` prefixes the key, `#` starts a comment unless it is
inside a word, so a URL fragment survives.

## What it does not do

No agent on the box during provisioning, no state file, no inventory, no
templating language. It reads the box and converges it. Anything that needs a
browser — a Tailscale login — is a `Manual` step that tells you the command and
checks whether you have run it yet.

## Status

v0.1. It provisions a Pi 4B running Raspberry Pi OS Lite (arm64) and has been
built and tested against that. Other Debian-family arm64 boards should work;
nothing in the steps is Pi-specific except the `vcgencmd` probes, which degrade
to `/sys` readings when it is absent.

## Licence

MIT.
