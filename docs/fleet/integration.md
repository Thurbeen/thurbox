# Installed integrations

Most agents take Fleet's hooks at launch: a flag or a variable names them,
and Fleet writes nothing in the agent's config directory
([harness profiles](controllers/task.md#harness-profiles)). Some agents read
hooks only from their own config files. For such an agent,
`fleet integration` writes Fleet's hooks into its config directory, and
removes them again.

An installed integration exists only for an agent with no launch-time way.
For every other agent, `fleet integration` answers `not needed: launch-time
hooks`.

## Commands

```text
fleet integration status [<agent>|--all] [--json]
fleet integration install <agent>
fleet integration uninstall <agent>|--all
```

- `status` reports one state for each agent. With no agent, it reports
  every harness Fleet has a profile for.
- `install` writes the agent's hooks. It replaces an older version that
  Fleet wrote.
- `uninstall` removes what Fleet wrote for the agent. `--all` removes every
  integration in the ledger.
- `fleet version --json` lists each outdated integration under
  `outdatedIntegrations`.

The commands work on this machine only. Fleet does not install an
integration on a remote host. To use an installed agent on a remote host,
run `fleet integration install <agent>` on that host.

## The agents

| Agent | Harness | Config directory | What Fleet writes |
| --- | --- | --- | --- |
| Kimi Code | `kimi` | `$KIMI_CODE_HOME`, else `~/.kimi-code` | a block at the end of `config.toml` |

Kimi Code reads hooks from its `config.toml` only. `KIMI_CODE_HOME` moves
its whole home, its sign-in included, so Fleet cannot name another
directory at launch.

## What Fleet writes

Fleet writes only what the profile's `[install]` table lists
([profile format](controllers/task.md#profile-format)). Each item is one of
these:

- **An own file.** Fleet owns the whole file. The agent must merge it with
  the user's files, for example one file in a hooks directory.
- **A block.** Fleet appends the block to a file that the user also edits.
  A first and a last comment line name Fleet:

  ```toml
  # >>> fleet integration kimi, version 1: `fleet integration uninstall kimi` removes this block
  [[hooks]]
  event = "Stop"
  command = '[ -z "$FLEET_SIGNAL_FILE" ] || fleet signal idle --harness kimi 2>/dev/null || true'
  timeout = 10
  # <<< fleet integration kimi
  ```

Each hook command obeys these rules:

- It exits at once when `FLEET_SIGNAL_FILE` is not set. Outside a Fleet
  session, the hook does nothing.
- It ends in `|| true`. A missing `fleet` never fails the agent's turn.

## The rules

- Fleet never creates the agent's config directory. When the directory is
  not there, `install` stops. Start the agent once, then install.
- Fleet reads the config directory from the agent's own variable, for
  example `KIMI_CODE_HOME`. Without the variable, it uses the agent's
  default directory under the home directory.
- Fleet does not take over a file that it did not write. When an own file
  is already there, `install` stops and writes nothing.
- Each write replaces the whole file:
  1. Fleet refuses a file with more than one hard link.
  2. Fleet follows a symbolic link to its real file. It refuses a file that
     is not a regular file.
  3. Fleet writes a temporary file in the same directory, with the
     permissions of the file it replaces.
  4. Fleet renames the temporary file over the file.
- On Windows, a rename can lose the file's access control list. So Fleet
  writes an existing file in place, and keeps a backup until the write is
  done:
  1. Fleet stops when `<file>.fleet-backup` or
     `<file>.fleet-backup.pending` is there. An earlier write was
     interrupted, and that backup can be the only good copy.
  2. Fleet copies the file to `<file>.fleet-backup.pending`, then renames
     the copy to `<file>.fleet-backup`.
  3. Fleet writes the file.
  4. Fleet removes the backup.

## The ledger

Fleet records each integration in `integrations.json`, in its data
directory. For each agent, the ledger holds:

- the version written, and the config directory;
- each own file, with a hash of its bytes;
- each block, with a hash of its bytes and the separator Fleet put before
  it;
- each directory that Fleet created.

Fleet writes the ledger entry before it writes the files. An interrupted
install is thus in the ledger, and `uninstall` removes it.

`uninstall` removes exactly what the ledger lists:

1. It removes each own file.
2. It removes each block, and the separator before it. The user's bytes
   stay as they were.
3. It removes a file that Fleet created for a block, when the file is
   empty without the block.
4. It removes each directory that Fleet created, when the directory is
   empty.

## The states

| State | Meaning | To do |
| --- | --- | --- |
| `not-needed` | The agent takes its hooks at launch. | Nothing. |
| `unsupported` | Fleet knows no hooks for the agent. | Nothing. |
| `agent-missing` | The agent's config directory is not there. | Start the agent once. |
| `not-installed` | The ledger lists no integration for the agent. | `fleet integration install <agent>` |
| `current` | The ledger version is the profile's, and each item is as Fleet wrote it. | Nothing. |
| `outdated` | The profile has a newer version, an item is missing, or the agent now reads another config directory. | `fleet integration install <agent>` |
| `modified` | A file or a block that Fleet wrote holds other bytes now. | `fleet integration uninstall <agent>`, then install again. |

`install` refuses a `modified` integration: it does not overwrite a
person's edit. `uninstall` removes the modified file or block.
