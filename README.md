# tdrive-rs

A Rust + GPUI rewrite of the TDrive desktop shell: the same dashboard, drive
sidebar, file table and trash as the Svelte application it is ported from, on
the `gpui` renderer instead of a webview.

The starting point is the original's own stylesheets, not a fresh design.
`frontend/src/ui/AppShell.svelte`, `frontend/src/ui/file-list/FileList.svelte`,
`frontend/src/styles/{tokens,chrome,files,trash}.css` and
`frontend/src/ui/theme/theme-palettes.css` are the specification; `src/theme.rs`
maps their custom properties one-for-one, so a rule that reads
`color: var(--text-muted)` becomes `theme::text_muted()`.

## Run

```bash
cargo run
```

Sign in with Telegram API credentials from my.telegram.org, then a phone number,
the login code and — if the account has one — the two-factor password.

### Where it keeps its data

The port reads and writes the same directory the original does, so both can be
pointed at one account without stepping on each other:

| File | Purpose |
| --- | --- |
| `<data dir>/imp_config.json` | `API_ID` / `API_HASH`, the same file the original's import flow writes. Created `0600` inside a `0700` directory. |
| `<data dir>/config.json` | The active drive. The original's own `channel_id` key is understood; the port additionally stores a `drives` array of the drives it has adopted, which the original ignores. |
| `<data dir>/tdrive.session` | The grammers session, in its own format, alongside the original's `session.json`. |

`<data dir>` is the platform data directory for `TDrive` (on Linux
`$XDG_DATA_HOME/TDrive`). The port's own `ProjectDirs` location is only a
fallback, used by demo mode.

### Choosing the drive

A drive is a **broadcast channel the account created**. Private chats, groups,
Saved Messages and channels the user merely joined are rejected in the transport
layer, so they never reach the picker at all — that is what keeps unrelated
conversations out of the app, rather than hiding them in the sidebar.

When nothing is saved yet, the port asks which channel is the drive, following
the original's `PersonalDriveSetup`: the candidates with their
"In use / Empty · Created <date> · ID <id>" line, a Create New TDrive flow
behind its own confirmation, and Continue disabled until a pick is made. The
choice is saved, so later launches open straight into the drive.

### Encryption

A drive that carries an `encfg` operation is encrypted. The port reads the
config — salt, KDF parameters, wrapped master key, key check and hint — from the
drive itself, and asks for the passphrase when an encrypted file is first
touched: a locked row shows a lock, and activating it raises "Unlock encrypted
files" with the drive's own hint. Argon2id derives the key from the passphrase,
unwraps the master key and verifies the key check, byte for byte the same
construction as the original (the format tests use material produced by the Go
implementation). The unlocked key lives in memory for the session and is
forgotten on a drive switch or logout.

### Environment variables

| Variable | Effect |
| --- | --- |
| `TDRIVE_DEMO=1` | Fills the shell with representative drives and files so the shell, sorting, search and the whole trash cycle can be walked without credentials. Mutating actions take a local path; nothing is sent to Telegram. |
| `TDRIVE_DEMO_PICKER=1` | Starts the demo at the drive picker, to walk the first-run flow. |
| `TDRIVE_DEMO_BULK=N` | Seeds N rows instead of the handful, for scroll and layout checks. |
| `TDRIVE_THEME=tokyo-night` | Swaps the alternate dark palette in. Quiet Relay is the default, matching `DEFAULT_THEME_PREFERENCE.darkThemeId`. |
| `TDRIVE_TRACE_OPS=1` | Prints one line about the replay to stderr — operations, objects, folders, encrypted, trashed, orphaned parents, unresolved bodies — plus a count per operation type. Numbers and object relationships only, never names or contents. A development aid for finding holes in the replay. |

## What is implemented

- **Shell** — 240px sidebar with the drive list, Photos and Trash destinations,
  the New shared drive / Join with link controls and the storage footer; header
  with search, reload and upload; breadcrumb; the sortable Name / Date / Size /
  Actions table; empty, loading-skeleton and error-banner states.
- **Login** — API credentials, phone, code and 2FA, each with its own step,
  inline error and in-flight message.
- **Drive picker** — see above.
- **Folders and files** — the drive's whole operation log is replayed into a
  tree rather than assuming one message per file: `t=f` uploads, `mkdir` and
  `dcommit` folders (an object's id is taken from the operation, because a
  folder created by a mount is `d:<uuid>` rather than the commit message's
  number), `meta` / `rename` / `move` updates, multipart groups (`part` plus
  `manifest`), the writable-mount ops (`fcommit`, `freplace`, `relocate`),
  trash (`trash`, `untrash`, `harddel`) and the encryption config. Folders open
  and list their contents, however deep.
- **Activation** — a single click selects, a double click activates: a folder
  opens, a file downloads, exactly the split the original's `activateRow`
  describes.
- **Download** — a row's content reference is resolved to the message, or
  messages, that actually hold the bytes, and chunk streams are written straight
  to disk. An encrypted file is decrypted through a staged sidecar, so a wrong
  key or a truncated transfer never leaves a half-written file behind, and a
  wrong passphrase reports itself instead of corrupting the destination.
- **Upload and delete** — uploads are sent as `t=f` operations, which the
  original reads; delete removes the operation message after the row's inline
  confirmation.
- **Trash** — the drive's own trash is read from its operations, so items
  deleted by the original app appear here with their origin and countdown, and
  the session's own deletions join them. Restore and purge act on the list.
- **Sorting, search and Photos** — the column headers sort either way, the
  search box filters by name, and Photos narrows the active drive to images.

## Not implemented

Stated plainly, because the original does all of this and this port does not:

- **Writes are minimal, and the trash's own writes are session-local.** Uploading
  sends a `t=f` operation and deleting removes the operation message; the
  writable-mount operations are parsed and replayed but never written. Purge and
  restore therefore change the list in front of you: for an entry the *drive*
  trashed, the drive's own `trash` operation still stands, so it is in the trash
  again on the next load. Emitting `untrash` / `harddel` / `relocate` — the
  formatter already supports them — is the next step.
- **The cold start re-reads the whole history.** Telegram serves history 100
  messages at a time, so a 6,571-message drive costs about 66 sequential
  requests. Measured on the test box: 42–57 s wall, 250 ms of CPU, so the wait
  is network round-trips. The original keeps a SQLite projection and syncs
  incrementally; this port has no cache yet (`src/db.rs` holds the schema for
  the durable version).
- **Search is folder-scoped.** The original searches the whole drive and hands
  the result to its own callback; here a query filters the folder you are in.
- **Gallery, viewers, WebDAV mount, photo backup, transfers, desktop updates** —
  none of it.
- **New shared drive / Join with link** report that they are unconnected rather
  than opening their flows.

## Deliberate differences from the Svelte shell

Each of these is a decision, not an omission:

- **The product mark is vector.** The original draws a raster
  `tdrive-logo.png`; this port draws the paper-plane glyph on an accent tile so
  the binary needs no image asset.
- **Row actions confirm inline.** The original opens a modal for a delete. Here
  the row's Download / Trash pair swaps to a Check / X pair in place, which keeps
  the confirmation on the item being confirmed.
- **The row "more actions" ellipsis is not ported.** Its menu is a second surface
  for the two actions the row already offers, so the row stays two buttons wide.

Everything else — including the quiet-until-hovered row actions, the 2fr/1fr/1fr
column ratio, the 44px rows, the 32px leading chips and the no-per-type-hue rule
(only folders carry the accent; the glyph carries the file type) — follows the
original.

## Performance

Scrolling is windowed by height the way the original's `row-window.ts` is, and
the row order and drive-wide size are memoised per `rows_version`, so neither
the search box, the sort, the folder nor the contents recompute per frame.
Measured on the real drive (5,724 objects, 1,181 of them in one folder), release
build:

| Case | Cost per frame |
| --- | --- |
| Folder with 3 rows | 53 ms |
| Folder with 1,181 rows | 53 ms |
| Same view with the window shrunk to 700×480 | 20 ms |
| This port's share of a frame: view render (160 µs) + building the ~18 visible rows (222 µs) | ~0.4 ms |

The frame cost is flat in the number of files and roughly proportional to the
*area* of the window, which is what a rasterizer costs and not what a list
costs: the test box has no `/dev/dri`, so GPUI falls back to software
rasterization (llvmpipe) and pays for 1.36 M pixels every frame. The list itself
is a couple of dozen rows and a few hundred microseconds, whatever the drive
holds. A machine with working GPU rendering does not pay that rasterization
cost.

## Layout

| File | Responsibility |
| --- | --- |
| `src/main.rs` | Window and application entry; the bundled icon set. |
| `src/ui.rs` | The whole shell: login, drive picker, sidebar, header, breadcrumb, table, rows, trash, the vault prompt and the action handlers. |
| `src/theme.rs` | Palette and metrics, mapped from the original's custom properties. |
| `src/models.rs` | Application state, the file and trash models, sorting, filtering and the session vault. |
| `src/format.rs` | `formatBytes`, `formatDate` and the trash countdown wording, matching `frontend/src/utils.ts` and `ui/trash/trash-view.ts`. |
| `src/ops.rs` | The `TDX1` operation wire format: parse and format, including the versioned writable-mount operations, with the same validation the original's `backend/projection/wire.go` applies. |
| `src/crypto.rs` | The vault: Argon2id, the wrapped master key, the key check and the XChaCha20-Poly1305 stream format. |
| `src/tele.rs` | The Telegram transport: connect, drive discovery, the operation replay, listing, download, upload, delete. |
| `src/config.rs` | The shared data directory and the original's `config.json` / `imp_config.json` layout. |
| `src/db.rs` | The local metadata schema. Not wired up yet. |

## Verification

```bash
cargo test    # 77 tests
cargo build   # no warnings
```

The tests cover the parts where being wrong is silent: the crypto format against
material the Go implementation produced, the `TDX1` round trip for every
operation including the writable-mount ones and the malformed headers Go
rejects, the replay rules (a folder commit resolving the files under it, a
trash taking a subtree and a restore putting back only what its own operation
took, an overwriting move trashing its victim, a hard delete purging from the
live tree or the trash, multipart parts resolving in index order, a replacement
repointing a row), the drive-selection rule, and the formatting and countdown
wording the original's tests pin.

The shell was walked on an X11 desktop by driving the built binary with
`xdotool` and capturing screenshots — the dashboard, Photos, the sorts, search,
the hover-revealed row actions, the inline delete confirm, the trash, the login
steps, the drive picker and the vault prompt — and then against the **real
drive**: the picker listed exactly one candidate (the account's own broadcast
channel, no personal chats), Continue adopted it, the replay produced 5,724
objects in 147 folders with **no object whose parent is missing**, the folders
listed their real contents several levels down, the drive's own trashed item
appeared in the trash, and activating an encrypted file raised the unlock prompt
carrying the drive's own hint, with a wrong passphrase rejected and reported
rather than accepted.

Not verified, and worth saying so: **an actual download and decrypt**, because
that drive is entirely encrypted (all 5,577 files carry `enc=1`) and its
passphrase is not available here; and **the write path** (upload, delete,
restore) against the original's projection, which would write into a real drive.
