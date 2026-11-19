# Links

A link connects two Parrot runtimes. Each sees the other's environment and offered sessions. They authenticate each other by key and encrypt everything end to end, so the carrier between them can be a third party's relay.

The carrier carries bytes only:

| Carrier | Channel | Containment |
| --- | --- | --- |
| `ssh` | `ssh <destination> parrot connect --link` with the user's SSH config | `shell`, unless the peer restricts the key to `parrot connect --link` |
| `command` | Any command whose standard input and output reach the peer's `parrot connect --link`, such as `docker exec -i` | `shell` |
| `tcp` | A TCP connection to `parrot serve --link-listen <addr>` | `restricted` |
| Carrier egg | A platform's own channel, such as a control plane or a message relay | As declared, else `unknown`, treated as `shell` |

`restricted` means the carrier can only run `parrot connect --link` on the machine. `shell` means whoever holds the carrier's credentials is the owner, whatever grant the link has, and clients show such a link as full access.

## Pairing

Each runtime has a static key in `link.key` and a runtime id derived from it. A link record pins the peer's key at pairing. A key that changes later fails the handshake and nothing is retried until the owner acts.

### With a code

On one runtime:

```bash
parrot serve --link-listen 0.0.0.0:7421
parrot link invite --name laptop --grant read,prompt --sessions linked
```

`invite` prints a one-time code, valid for ten minutes. On the other runtime:

```bash
parrot link add --name studio --tcp 192.0.2.10:7421 --code-stdin
```

Paste the code on standard input. The command takes the code from standard input because a code is a secret. `--grant` is what the peer may do here, `--sessions` is what it sees here and `--offer-env` offers this environment.

### Over the carrier

When the carrier gives the dialling user a shell on the peer, as SSH usually does, no code has to be copied:

```bash
parrot link add --name studio --ssh ops@192.0.2.10 \
  --grant read --sessions none \
  --ask-grant read,prompt --ask-sessions linked
```

`--ask-grant` and `--ask-sessions` are what this runtime asks the peer to give; `--grant` and `--sessions` are what it gives in return. The dialling runtime runs `parrot link invite --json` on the peer through the carrier, then pairs with the code it gets back. The carrier is trusted once, for that exchange.

For an SSH key that can only relay a link, name the peer's public key:

```bash
parrot link invite --name laptop --grant read,prompt --authorized-key "ssh-ed25519 AAAA… peer@example.invalid"
```

The key is written to `~/.ssh/authorized_keys` with `command="parrot connect --link",restrict`, and any other line for the same key is removed. The link then reports `restricted` rather than `shell`. Use `--authorized-keys <file>` for another file.

## Grants and offers

A link record has two directions. `grant` is what the peer may do on this runtime. `offer` is what the peer sees: its environment and `none`, `linked` or `all` of its sessions. `linked` means only sessions created over the link.

| Method | Does |
| --- | --- |
| `link/list` | Lists the links, their status and current carrier |
| `link/invite` | Makes an invite and its one-time code |
| `link/add` | Dials, pairs and records the link |
| `link/update` | Changes the carrier, grant or offer |
| `link/remove` | Removes the link and revokes pairing |

`parrot link list` prints the records. `parrot link remove <link id>` removes one. The commands talk to the local runtime, and `parrot link` is not available on Windows yet.

## How a link behaves

- **No transitivity.** A runtime shows only its own environment and sessions, never a peer's peers.
- **Ownership.** A session belongs to the runtime whose environment runs the binding. Calls for a session go to its owner; a client of another runtime reaches it through the link, narrowed by the link's grant.
- **Failover.** When a connection drops, the dialling side tries the carrier, then its `fallback` list, with backoff from one second to five minutes.
- **Reconnects.** Subscriptions resume with `replayFrom`. The environment shows `offline` while the link is down.
- **Credentials.** Agent CLI sign-in is per agent and environment. Git credentials stay with each environment. Only protocol messages, bundles, git bundles and patches cross a link, encrypted.

`link/update` can set a carrier list with `fallback`, so a laptop tries SSH and then a relay. `carrier/list` lists the carriers this runtime knows, including those from carrier eggs.

## What works today

A link pairs, lists, updates and removes. Through it:

- `session/new` with the peer's environment creates a root session there, when the link offers its sessions. A child in a linked environment answers `unsupported` with `tree.remote`.
- `workspace/transfer` sends a workspace to the peer. [Workspaces](workspaces.md#moving-a-workspace) has the rules.
- Calls and subscriptions for a session the peer owns are forwarded over the link.

Moving an existing session to the peer's environment does not work yet: `session/rebind` with another environment answers `unsupported` with `profile.link`.

## Carrier eggs

A carrier egg is a Hatchery package with the `parrot` target and a `carrier.kdl` instead of a `driver.kdl`. It lets a platform's own channel carry a link where SSH, a command or TCP can't reach the machine. The runtime starts `<program> dial` for each connection, or `<program> listen` beside itself for inbound channels.

The file format, the `dial` and `listen` contract and the containment rules are in [Writing a driver](writing-drivers.md#carrier-eggs).
