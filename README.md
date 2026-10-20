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
| `<data dir>/projection/<drive_id>.json` | The port's own snapshot of the drive's projection, so a launch resumes instead of replaying the whole history. A cache, never the authority: it is written after every read, and an unreadable or older-format file is ignored and rebuilt. See `src/cache.rs`. |

`<data dir>` is the platform data directory for `TDrive` (on Linux
`$XDG_DATA_HOME/TDrive`). The port's own `ProjectDirs` location is only a
fallback, used by demo mode.

When the original application has already signed in, the port reads its
`session.json` and imports that authorisation into its own session — read-only,
and only when it has no session of its own — so an existing install does not
have to be signed in twice. It never calls `logOut` on an imported session, and
it never asks Telegram for a login code for an account that is already
authorised.

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
| `TDRIVE_FULL_LOAD=1` | Ignores the snapshot and replays the drive's whole history. The same thing the header's Reload button does, and the way out if a snapshot is ever wrong. |
| `TDRIVE_TRACE_OPS=1` | Prints one line about the replay to stderr — operations, objects, folders, encrypted, trashed, orphaned parents, unresolved bodies — plus a count per operation type. Numbers and object relationships only, never names or contents. A development aid for finding holes in the replay. |
| `TDRIVE_UPLOAD=<path>` | Sends that file to the drive once it opens, so the upload path can be walked where there is no file dialog to pick a file with. A development aid. |

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
  original reads. Every upload opens the original's "How would you like to
  upload?" choice, so a batch can be encrypted before it is sent; a drive with
  no encryption config yet is walked through the original's password setup
  first, and the master key it creates stays in memory for the session. Delete
  is the drive's own trash, not a Telegram deletion: the port publishes the
  `trash` operation the original publishes, so the file keeps its bytes and its
  retention window and is restorable from either app.
- **Trash** — the drive's own trash is read from its operations, so items
  deleted by the original app appear here with their origin and countdown, and
  the port's own deletions join them. Restore and purge are written back as
  `untrash` and `harddel` operations, each naming the revision the drive is
  holding, so what the drive did and what is on screen are the same thing.
- **Sorting, search and Photos** — the column headers sort either way, the
  search box searches the whole drive (folders first, each half in the column's
  own order, with the result's path where the date goes), and Photos narrows the
  active drive to images.
- **Gallery** — the drive's pictures and videos, grouped into a section per
  folder, in a windowed list that measures each row rather than assuming they
  are all the same height. A plain file is shown from Telegram's own rendition,
  which costs a few kilobytes. An encrypted file has no rendition of its own,
  because the bytes on Telegram are ciphertext, so its tile carries a
  "Load preview" button that says what the picture costs; only that one file is
  fetched when it is pressed, the picture is made from it at 512 pixels, and the
  file itself is thrown away, so the cache holds pictures rather than a second
  copy of the drive. A file too large to be worth a picture is not fetched at
  all. On the real drive (5,724 objects, 7.4 GB, every file encrypted) opening
  and scrolling the gallery costs 66 bytes.
- **Viewing a file** — a double click opens it: a picture is drawn, a text file
  is shown as text, and anything else is saved. The file's name and kind head
  the viewer, as they do in the original's.
- **Multi-select** — a plain click picks one row, ctrl or cmd click toggles one,
  shift click takes the range, and the selection is held by the drive's own
  object id, so it survives the list being rebuilt. The selection bar sits over
  the table header, with the count on the left and Download, Move, Delete and
  Clear on the right.
- **New folder, rename and move** — a folder is created with the drive's own
  `mkdir`, and a file or folder is renamed and moved with the classic `rename`
  and `move` operations, so the original reads them as it does its own.
- **Appearance** — the account menu at the end of the header opens the
  original's appearance panel: a light and a dark mode, and the catalogue of
  seventeen palettes, each one the original's own colours read out of its
  `theme-palettes.css`. A palette card draws the theme's own picture of the
  shell, and the chosen theme is remembered beside the drive list, so the next
  launch opens in it.
- **Snapshot** — the drive's projection is kept beside the original's files, so
  a launch replays only the operations that arrived since the last one. On the
  test drive (6,571 messages, 5,724 objects) a full replay costs 64 s and a
  resumed launch 0.7 s. The snapshot is a cache: it is rebuilt whenever the
  drive disagrees with it, and `cargo test` pins that an unreadable or
  older-format file is ignored rather than misread.

## Not implemented

Stated plainly, because the original does all of this and this port does not:

- **The rest of the write surface.** The writable-mount operations (`fcommit`,
  `freplace`, `relocate`) are replayed but never written: uploads still send
  the single-message `t=f` form, encrypted or plain.
- **WebDAV mount, photo backup, transfers, desktop updates** — none of them.
- **New shared drive / Join with link** report that they are unconnected rather
  than opening their flows.
- **Notifications** — none of them.

## Deliberate differences from the Svelte shell

Each of these is a decision, not an omission:

- **The product mark is vector.** The original draws a raster
  `tdrive-logo.png`; this port draws the paper-plane glyph on an accent tile so
  the binary needs no image asset.
- **Row actions confirm inline.** The original opens a modal for a delete. Here
  the row's Download / Trash pair swaps to a Check / X pair in place, which keeps
  the confirmation on the item being confirmed.
- **Search results follow the live sort.** The original runs one SQL query whose
  order is fixed; here each half of the result is ordered by the column the list
  is sorted by, so the headers stay live while a search is on screen. The
  contents are the original's: folders first, one per kind, and the path in the
  Date column's place.
- **The row "more actions" ellipsis is not ported.** Its menu is a second surface
  for the two actions the row already offers, so the row stays two buttons wide.
- **The gallery asks before it spends the reader's connection.** The original
  runs its own rendition service, so its client only ever fetches a small
  preview; this port has no server, so a picture of an encrypted file costs the
  whole file and the gallery says so on the tile rather than pulling the drive
  over the connection as the reader scrolls.

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
| `src/palettes.rs` | The original's seventeen palettes, one constant each, read out of its `theme-palettes.css`. |
| `src/rendition.rs` | The small pictures published beside a file: made from its bytes, sealed for an encrypted drive, and read back only for the file they were made from. |
| `src/models.rs` | Application state, the file and trash models, sorting, filtering and the session vault. |
| `src/format.rs` | `formatBytes`, `formatDate` and the trash countdown wording, matching `frontend/src/utils.ts` and `ui/trash/trash-view.ts`. |
| `src/ops.rs` | The `TDX1` operation wire format: parse and format, including the versioned writable-mount operations, with the same validation the original's `backend/projection/wire.go` applies. |
| `src/crypto.rs` | The vault: Argon2id, the wrapped master key, the key check and the XChaCha20-Poly1305 stream format. |
| `src/tele.rs` | The Telegram transport: connect, session import, drive discovery, the operation replay, listing, download, upload, and the operations a delete, a restore and a purge publish. |
| `src/cache.rs` | The snapshot of a drive's projection that a launch resumes from. |
| `src/config.rs` | The shared data directory and the original's `config.json` / `imp_config.json` layout. |
| `src/db.rs` | The local metadata schema. Not wired up: `src/cache.rs` covers what the shell needs. |

## Verification

```bash
cargo test    # 95 tests
cargo build   # no warnings
```

The tests cover the parts where being wrong is silent: the crypto format against
material the Go implementation produced, the `TDX1` round trip for every
operation including the writable-mount ones and the malformed headers Go
rejects, the op id a delete derives (pinned to the value the original computes
for the same input), the replay rules (a folder commit resolving the files under
it, a trash taking a subtree and a restore putting back only what its own
operation took, an overwriting move trashing its victim, a hard delete purging
from the live tree or the trash, multipart parts resolving in index order, a
replacement repointing a row), the revision every operation leaves behind, which
bodies a purge destroys, the drive-selection rule, and the formatting and
countdown wording the original's tests pin.

The write path was then checked against the original's own projection rather
than against a drive: its reader was handed the exact headers this port writes —
pinned in `src/ops.rs` as text — and asked to apply them to a fresh schema. It
creates the file at revision 1, accepts the delete, advances the row to the
revision the restore names, records the trash entry with the retention window
the header carries, treats a replayed delete as the same operation rather than a
second one, refuses a delete built on a stale revision, and purges the entry
when the marker names the revision the tombstone left. That is the whole
contract between the two implementations, checked without writing to a drive.

The revisions themselves were checked against the real drive's SQLite
projection: every one of the 5,364 objects this port had in common with it, and
all 12 members of the drive's trashed subtree, carried the same revision in both
(the rest of the drive's objects postdate that copy of the projection). A
revision that is wrong is not a visible error — the drive simply refuses the
write — so this is the check that says the deletes work at all.

The shell was walked on an X11 desktop by driving the built binary with
`xdotool` and capturing screenshots — the dashboard, Photos, the sorts, search,
the hover-revealed row actions, the inline delete confirm, the trash, the login
steps, the drive picker and the vault prompt — and then against the **real
drive**, signed in on the original's own authorisation: the picker listed
exactly one candidate (the account's own broadcast channel, no personal chats),
Continue adopted it, the replay produced 5,724 objects in 147 folders with **no
object whose parent is missing**, the folders listed their real contents several
levels down, a drive-wide search found files in deep folders and landed on them,
the drive's own trashed item appeared in the trash, and activating an encrypted
file raised the unlock prompt carrying the drive's own hint, with a wrong
passphrase rejected and reported rather than accepted. The port's own session,
`imp_config.json` and snapshots were removed from that machine afterwards.

Not verified, and worth saying so: **an actual download and decrypt**, because
that drive is entirely encrypted (all 5,577 files carry `enc=1`) and its
passphrase is not available here; and **a write against the real drive** — the
delete, restore and purge cycle was checked against the original's projection
and against the drive's own revision state, but actually publishing one into
that drive would have put the account's data behind an operation no one asked
for.
