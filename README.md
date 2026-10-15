# tdrive-rs

A Rust + GPUI rewrite of the TDrive desktop shell: the same dashboard, drive
sidebar, file table and trash as the Svelte application in this repository, on
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
the login code and — if the account has one — the two-factor password. The
grammers session file and the local data directory follow the platform's
`ProjectDirs` location.

Two environment variables help when there is no session to sign in with:

| Variable | Effect |
| --- | --- |
| `TDRIVE_DEMO=1` | Fills the shell with representative drives and files so the dashboard, Photos, sorting, search and the whole trash cycle can be walked without credentials. Mutating actions take a local path in this mode; nothing is sent to Telegram. |
| `TDRIVE_THEME=tokyo-night` | Swaps the alternate dark palette in. Quiet Relay is the default, matching `DEFAULT_THEME_PREFERENCE.darkThemeId`. |

## What is implemented

- **Shell** — 240px sidebar with the drive list, Photos and Trash destinations,
  the New shared drive / Join with link controls and the storage footer; header
  with search, reload and upload; breadcrumb; the sortable Name / Date / Size /
  Actions table; empty, loading-skeleton and error-banner states.
- **Login** — API credentials, phone, code and 2FA, each with its own step,
  inline error and in-flight message.
- **Files** — dialog list, per-drive listing, download to the platform downloads
  directory with per-row state (Downloading / Saved / Failed), upload through a
  native file dialog, and delete.
- **Trash** — deleted files move into the trash with their origin and a purge
  deadline; the trash shows a "Time left" countdown, restores items back into
  the drive, purges single items behind a confirm, and empties entirely behind a
  two-step arm.
- **Sorting, search and Photos** — the table's column headers sort ascending or
  descending, the search box filters by name, and the Photos destination narrows
  the active drive to images.

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

## Layout

| File | Responsibility |
| --- | --- |
| `src/main.rs` | Window and application entry; the bundled icon set. |
| `src/ui.rs` | The whole shell: login, sidebar, header, breadcrumb, table, rows, trash and the action handlers. |
| `src/theme.rs` | Palette and metrics, mapped from the original's custom properties. |
| `src/models.rs` | Application state, the file and trash models, sorting and filtering. |
| `src/format.rs` | `formatBytes`, `formatDate` and the trash countdown wording, matching `frontend/src/utils.ts` and `ui/trash/trash-view.ts`. |
| `src/tele.rs` | The Telegram transport: connect, dialogs, listing, download, upload, delete. |
| `src/db.rs` | The local metadata schema. Not wired up yet. |

## Not implemented

Stated plainly, because the original does all of this and this port does not:

- **The trash is session-scoped.** Deleting a file calls the Telegram delete, but
  the trashed copy and its purge deadline live in memory for the life of the
  process; restoring is a local move back into the list. `src/db.rs` holds the
  schema for the durable version.
- **Gallery, viewers, WebDAV mount, photo backup, encryption, transfers and
  updates** — none of it. The file table is the whole of the browsing surface.
- **Folder navigation.** Drives map to Telegram dialogs; the folder rows in the
  demo data are display-only.
- **New shared drive / Join with link** report that they are unconnected rather
  than opening their flows.

## Verification

```bash
cargo test    # format.rs: byte/date formatting and the countdown wording
cargo build   # no warnings
```

The interface was checked against the original on an X11 desktop by driving the
built binary with `xdotool` and capturing screenshots: the dashboard, Photos,
Date and Size sorts, search, the hover-revealed row actions, the inline delete
confirm, the trash (list, armed empty, per-row purge confirm, restore) and the
login screen. Upload is unverified — it needs a native file dialog and a live
session.
