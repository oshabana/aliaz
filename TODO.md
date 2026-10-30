# Aliaz work

## To do

- [ ] Deploy the command-details sync support after approval; existing hosted service support is not assumed.

- [ ] Qualify the sync migration and transactional uploads in an isolated D1 runtime before a hosted release; local SQLite tests do not establish hosted behavior.
- [ ] Make binary updates atomic and verify replacement of a running executable on Linux.
- [ ] Make an entire sync pull atomic when later encrypted records fail validation or decryption.

## Deferred

- [ ] Add automatic project-aware collections after the command-library flows are qualified.
- [ ] Consider background sync, command templates, and a separate full shell-history scope.

## Completed

- [x] Qualify actual command-library terminal paths on macOS: Bash 3.2 review/copy fallback, Bash 5.3 editable insertion, zsh, and fish; verify the save wizard preserves literal newlines. Full package verification is recorded separately by the lead.

- [x] Add a safe default picker and configurable shell shortcut with editable-buffer insertion.
- [x] Add descriptions, tags, favorites, and local recent-selection ordering.
- [x] Add editor-based saving, previous-command capture, and import previews.
- [x] Add grouped undo and inspectable, restorable conflict backups with collection identity.
- [x] Preserve command details separately during sync so older clients cannot erase them with command-only updates.

- [x] Track shell-owned definitions so deletion, deactivation, and fallback priority refresh in the current shell while preserving user overrides.
- [x] Reject new leading-dash and reserved aliaz alias names while keeping existing records removable.
- [x] Preserve alias identities and deletion records when collection moves encounter deleted aliases; resolve remote moves and name collisions with backups.
- [x] Roll back imports and collection activation when a later entry fails.
- [x] Keep the sync cursor at the pulled watermark, split large uploads, and preserve edits made while an upload is in flight.
- [x] Normalize login recovery phrases, save them after successful sync, and refresh installed shell files after login.
- [x] Keep update and uninstall available when the local database belongs to a newer release.
- [x] Scope server record identities to each account and allocate upload versions within one database transaction.
- [x] Probe the installer's controlling terminal, honor ZDOTDIR and bash login profiles, and repair commented shell source lines.

Completed entries describe implemented changes. The lead records consolidated verification separately.
