# Daemon lifecycle

LeIndex uses `leindexd` for the long-lived local daemon. The `leindex` CLI owns
its lifecycle; control words are subcommands, not arguments passed to the daemon.

## Commands

```bash
# Show state (also the default when no action is supplied)
leindex daemon status
leindex daemon

# Start, stop, or replace the user-scoped daemon
leindex daemon start
leindex daemon stop
leindex daemon restart
```

`status` reports one of three states:

- **running** — the endpoint sidecar matches the current protocol and a live
  `leindexd` process whose executable identity was verified.
- **stopped** — no endpoint sidecar exists.
- **stale** — metadata exists, but the process is gone, the protocol is old, or
  the endpoint does not identify `leindexd`.

The endpoint sidecar and Unix socket live under the `run` directory inside the
LeIndex home. By default that is `~/.leindex/run`; `LEINDEX_HOME` changes the
home location. `LEINDEXD_BIN` may be set when `start` must use a daemon binary
outside the directory containing `leindex`.

`stop` sends a graceful termination request, waits briefly for the process to
exit, and removes stale endpoint sidecars. `restart` performs `stop` followed
by `start`. Starting an already-running daemon is an error rather than a
second daemon spawn.

## Discoverability from the binary

The same information is available without opening this file:

```bash
leindex --help
leindex daemon --help
```

The top-level help lists `daemon`; the nested help lists `status`, `start`,
`stop`, and `restart` with their behavior.
