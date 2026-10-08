# molso usage contract

This document is the observable contract of the first version. It is written
for a trusted agent (and the person supervising it), not for an adversary.

## Naming

A credential name is exactly `<service>/<account>`:

- one `/` separating two non-empty segments;
- no control characters;
- valid UTF-8: credential names, `--env` variable names, and lookups must be
  valid UTF-8;
- not interpreted as a file path.

Command arguments after `exec --` are **not** forced to UTF-8; they are passed
through as platform strings (`OsString`), so arguments containing arbitrary
bytes are preserved.

A segment may start with `-`. Because such a token looks like an option, pass
it after `--`:

```sh
molso put --stdin -- -service/account
molso get -- -service/account
molso remove -- -service/account
```

## Values are bytes

Values are raw bytes, never strings:

- `put` stores exactly the bytes it receives.
- `get` writes exactly those bytes to stdout and appends nothing, not even a
  newline.
- `exec --stdin` writes exactly those bytes to the child stdin.

The on-disk plaintext is JSON, and each value is represented as an array of
byte values (`[0..255]`) so that arbitrary bytes survive without an encoding
round trip.

## Runtime and permissions

- Building requires Rust 1.89 or newer; `std::fs::File::lock` / `lock_shared`
  stabilized in 1.89.
- Unix: the key and vault are created with mode `0600`, the default data
  directory with `0700`, and the lock file with `0600`.
- Windows: the key, vault, directory, and lock file are created with a
  protected DACL granting full access only to the current user.

## Paths

`--vault` and `--key-file` are global options, accepted before or after a
subcommand. Both `--vault path` and `--vault=path` work. Options after the
`exec --` separator belong to the child command and are passed through.

Resolution precedence, per file:

1. CLI: `--vault <path>`, `--key-file <path>`
2. Environment: `MOLSO_VAULT`, `MOLSO_KEY_FILE` (paths only, never key material)
3. Platform local data directory: `dirs::data_local_dir()/molso/{key,vault}`

Paths are made absolute against the current working directory with
`std::path::absolute`; relative inputs remain relative to the invoking cwd. If
`--vault` and `--key-file` point to different directories, copying only the
vault will not decrypt.

## Commands

### `init`

Creates the key (32 random bytes) and an empty vault. Never overwrites an
existing key or vault. State handling:

| key | vault | result |
| --- | --- | --- |
| absent | absent | create both |
| present (full 32 bytes) | absent | reuse the key, create the vault |
| absent | present | error, never generate a new key |
| present | present | error, `already initialized` |

A key file whose length is not exactly 32 bytes is rejected and never used or
overwritten. `init` takes the same stable exclusive lock as other writes.
On a terminal, the key and vault paths have labels. Redirected stdout retains
two unlabelled lines, key first and vault second. Repeated initialization fails
with a hint to run `list` or `put`.

### `put <name> [--stdin [--allow-empty]] [--update]`

The store and create/update condition are checked before requesting or reading
the credential. Known errors therefore do not make a person enter a value or
an agent finish an input stream first. No vault lock is held while waiting for
input; the create/update condition is checked again under the write lock when
committing, so a concurrent change can still make the final write fail.

- Without `--stdin`, the credential is read from the terminal without echo; the
  terminating Enter is **not** stored. Empty terminal input fails with exit 2
  without creating or replacing an entry. If no terminal is available the command
  fails immediately, so an agent workflow does not hang waiting for input.
- With `--stdin`, the credential is read as raw bytes from stdin; a trailing
  newline is stored as-is. Empty input fails with exit 2 before any save,
  preventing a producer that fails without output from replacing an entry with
  an empty value. To intentionally store an empty value, pass `--allow-empty`,
  which requires `--stdin`. This is the form to use from an agent.
- A new entry is created by default. Replacing an existing entry requires
  `--update`; `--update` on a missing entry is an error. A `put` that fails
  before the commit point does not modify the vault; a failure after the commit
  point (for example the parent directory sync) reports durability as
  unconfirmed and the new content may already be in place (see Storage and
  concurrency).
- On success a byte count (never the value) is written to stderr.
- Unknown extra arguments are not echoed in diagnostics. For an option typo,
  a suggestion may show a declared option name, such as `--update`.

The producer's exit status is not visible to molso. A failed producer that
emits non-empty partial output can still cause a save; callers must check
producer success. A pipeline failure reported afterwards does not roll back
an already committed write.

### `list`

Prints one `service/account` per line, sorted by name. An empty vault is a
successful result with empty stdout. When stdout is a terminal, an empty
vault also produces a next-step hint on stderr. When stdout is redirected
or piped, an empty list produces no output on either stream.

### `get <name>`

- Refuses to run when stdout is a terminal.
- Writes raw bytes to stdout, no trailing newline.
- Errors go to stderr and never contain credential bytes.
- The vault lock is released after the credential is copied out and before any
  output, so a slow consumer does not block writers.

### `remove <name>`

Deletes an entry. Removing a missing entry is an error before the commit point,
so the vault is not replaced.

### `exec [--env VAR=name]... [--stdin name] -- <cmd> [args]...`

`--env=VAR=name` and `--stdin=name` are also accepted. The `--` separator is
required so the child's options, including `--help`, stay its own.

Without any `--env` or `--stdin` mapping, exec just runs the child without
opening a vault or key, and does not require initialization.

- Arguments after `--` are passed as an array. No shell is involved and the
  credential never appears in the child argv.
- `--env VAR=name` injects the value into the child environment. The value must
  be valid UTF-8 and contain no NUL. Duplicate variable names are rejected using
  the platform rule (case-insensitive on Windows, including Unicode; case-
  sensitive elsewhere), so a later mapping never silently overwrites one.
- `--stdin name` writes the raw bytes to the child stdin and then closes it.
  This is the recommended consumption path because it avoids shell pipelines.
- All mappings are validated before any child starts. A validation failure
  starts nothing.
- The vault lock is released after the referenced credentials are copied out
  and before the child is spawned or waited on, so a child that calls back into
  `molso` cannot deadlock its parent.

## Exit codes

| code | meaning |
| --- | --- |
| 0 | success |
| 2 | molso usage/storage/credential error (management commands and `get`) |
| 125 | `exec` wrapper failure: delivery failure or an unexpected spawn error |
| 126 | command found but not executable |
| 127 | command not found |
| other | passthrough of the child exit status, including 125/126/127 |

On Unix a child terminated by signal maps to `128 + signal`. On Windows the
child exit status is preserved as a 32-bit value.

`exec` semantics that are easy to get wrong:

- A successful write to the child stdin does **not** mean the child read or used
  the credential. The pipe has a kernel buffer, so a small payload can be
  accepted even if the child never reads it.
- If the write fails (broken pipe or other I/O error) and the child still exits
  with 0, `molso` returns 125 instead of 0, so a delivery failure is never
  reported as success. If the child exits non-zero, that status is passed
  through.
- `molso` always waits for the child, even after a delivery failure.

## Storage and concurrency

- A single stable lock file `<vault>.lock` (never replaced) serializes
  read-modify-write. Writers take it exclusively; readers take it shared.
- Updates are written to a same-directory temporary file, synced, renamed over
  the vault (the commit point), and the parent directory is synced on Unix.
- Failure reporting distinguishes: failure before commit (original unchanged),
  committed but directory sync failed (durability unconfirmed), process
  interruption, and real power loss. During normal operation the guarantee is
  atomic visibility: readers see either the complete old or the complete new
  vault, never a torn one. This is **not** a power-loss guarantee; actual
  recovery after power loss depends on the filesystem and storage and has not
  been verified.
- `init` publishes new files with a create-if-absent commit so an existing
  target is never overwritten. The key may briefly exist as a permission-
  protected plaintext temporary file; stale temporary files are not treated as
  a vault or key, and their absence after `SIGKILL` or power loss is not
  guaranteed.

## Explicit non-guarantees

- Refusing a terminal does not prove the consumer is safe; a non-TTY stdout can
  still be an agent log collector.
- No consumption-confirmation protocol: a zero exit does not prove the
  credential authenticated.
- Not all in-memory copies are erased. `molso` zeroizes its own buffers and
  enables the cipher's `zeroize` feature, but copies in the OS, the child
  environment block, and the kernel are outside its control.
- Shell pipeline byte fidelity is not universal. Prefer
  `molso exec --stdin`. In particular, older Windows PowerShell pipelines may
  transcode native command output through .NET strings; PowerShell 7.4+ has a
  native-to-native byte-preserving path, but cmdlets in between can still
  transcode.
- Crash/power-loss recovery depends on the filesystem; Windows directory
  persistence has not been verified natively.
- Native automated tests have passed on macOS locally and on Linux/Windows
  in CI. See [validation.md](validation.md) for the exact scope; this does not
  establish Windows console-input, shell-conversion, or power-loss guarantees.
