# Aliaz vision

Aliaz is a personal command library for terminal users who want to find and reuse commands by purpose.
A user saves a command without struggling with shell quoting, adds a description or tags,
then finds it in a searchable picker. The default selection puts the full command in the
shell's editable line where supported, so the user can review and change it before running.
Execution is always explicit. Shortcuts leave existing history controls alone.

Favorites and recent selections make frequent commands easy to find. Collections keep
predictable priority and let each computer choose which commands are active. Local undo
and inspectable conflict backups make accidental edits, deletions, and replacements
recoverable with their original command details and collection.

Local alias management works without an account or network access. Bash, zsh, and fish
integration should handle commands safely, refresh removed aliases, and respect aliases the
user changes themselves. Collections give predictable priority: shared first, then the
collections activated on this computer.

Optional encrypted sync carries alias and collection changes between machines while keeping
collection activation local. Failed imports and uploads must leave recoverable state, and
conflicting commands must be retained for recovery. Errors should explain what the user can
do next without exposing their commands or recovery phrase.

The first command-library release covers safe selection, purpose-based search, easy saving,
and recovery. Automatic project collections, background sync, command templates, and a
full shell-history product remain outside this release.
