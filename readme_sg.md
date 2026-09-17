# recall: changes in this copy

This copy of [zippoxer/recall](https://github.com/zippoxer/recall) adds
project scoping that includes git worktrees, word-start search, a full-screen
transcript view, nvim-style keys, and filters. This file lists everything that
differs from upstream (commit `e605ab9`). The main [README](README.md) has the
full key tables.

## Install

Build and install from this folder (replaces any Homebrew `recall`):

```bash
brew uninstall recall   # if installed
cargo install --path ~/Programming/recall
```

If fetching the `crossterm` git dependency fails with "no authentication
methods succeeded", use the git command line for fetching:

```bash
CARGO_NET_GIT_FETCH_WITH_CLI=true cargo install --path ~/Programming/recall
```

The first start after installing rebuilds the index (about 8 seconds for
2,500 sessions). The index is rebuilt automatically whenever its format changes.

## Why

Upstream recall is fast, but:

- started in a project folder, it often showed nothing, because the folder
  filter ran after the 200-result limit and claude-mem observer sessions filled
  those 200 places
- sessions in git worktrees did not count as part of their project
- search only matched whole words
- it had no transcript view, titles, or date filter

## Changes

### Which sessions are indexed

- **claude-mem observer sessions are skipped.** Files whose path contains
  `claude-mem-observer-sessions` are not indexed. The index went from 547 MB to
  57 MB.
- **Automated sessions can be skipped.** Sessions whose first message starts
  with a configured string (e.g. scheduled cron jobs) are not indexed.
- **Removed and skipped files leave the index.** Before, a deleted session file
  stayed searchable.
- Both lists live in `~/.config/recall/config.toml`. Changing
  `skip_sessions_starting_with` makes recall index every session again on its
  next start.

```toml
skip_paths_containing = ["claude-mem-observer-sessions"]
skip_sessions_starting_with = [
  "[IMPORTANT: You are running as a scheduled cron job",
  "Review the conversation above and update the skill library",
]
```

### Project scope

- The project filter runs inside the search index, before the result limit, so
  a project with older sessions still shows them.
- A project includes its subfolders and its git worktrees:
  `<project>__worktrees/<branch>` (workmux) and `<project>/.worktrees/<branch>`.
- `recall search --cwd` and `recall list --cwd` use the same rule.
- `--source`, `--since` and `--until` also filter inside the index.

### Search

- Typed words match the start of words: `xeni` finds `xenia`. Every word must
  match. Whole-word matches rank higher.
- Quotes still search for an exact phrase: `"gunicorn restart"`.
- Conversation titles are searchable.
- Date words in the search box: `since:2w`, `after:2025-12-01`,
  `until:yesterday`, `before:3d`. Units are `m` `h` `d` `w` `mo` `y`, or
  `today`, `yesterday`, or a date. The CLI `--since`/`--until` accept the same
  values. `today` and `yesterday` mean the start of that day.
- Tool filter: `t` (or `Ctrl+S`) steps through all, Claude, Codex, Factory,
  OpenCode.

### Session list

- One row per session in the recent list (upstream could repeat a session).
- Each row shows the project name, `⎇ <worktree>` for worktree sessions, the
  tool, how long ago, and the conversation title (Claude Code `/rename` or its
  generated title, Codex thread name, OpenCode/Factory title).
- The preview on the right reads a session file once and reuses it until the
  file changes (upstream read it on every redraw).

### nvim-style keys

recall opens in **normal mode**, where letters are commands. `/` or `i` starts
typing a search; `Enter` or `Esc` finishes it and keeps the text.

| Where | Keys |
|-------|------|
| Session list | `j` `k` move, `g` `G` first/last, `Ctrl+D` `Ctrl+U` `Ctrl+F` `Ctrl+B` pages, `Enter` open transcript, `Ctrl+R` resume, `s` project/everywhere, `t` tool filter, `y` copy session ID, `Y` copy resume command, `Tab` preview, `Esc` clear search |
| Preview (after `Tab` or `Ctrl+W w`) | `j` `k` scroll, `g` `G` first/last message, `]` `[` next/previous message, `o` expand, `Enter` open transcript at this message, `Tab` or `Esc` back to the list |
| Typing a search | `↑` `↓` move, `Ctrl+W` delete word, `Ctrl+U` delete to start, `Ctrl+A` `Ctrl+E` start/end |
| Transcript view | `j` `k` line, `d` `u` half page, `f` `b` `Space` full page, `g` `G` top/bottom, `]` `[` message, `}` `{` your messages, `/` search with `n` `N`, `Ctrl+R` resume, `y` `Y` copy, `q` back |
| Everywhere | `?` or `F1` shortcuts panel, `Ctrl+C` quit |

Keys that changed from upstream:

| Action | Upstream | Now |
|--------|----------|-----|
| Resume | `Enter` | `Ctrl+R` (`Enter` opens the transcript) |
| Project / everywhere | `/` | `s` (`/` types a search) |
| Copy session ID | `Tab` (and quit) | `y` (stays open) |
| Quit | `Esc` on an empty search | `Ctrl+C` only; `Esc` and `q` never quit |

### Transcript view

- `Enter` on a session opens the whole conversation, full width, with every
  message in full.
- It opens at the matching message when you searched, at the focused message
  when opened from the preview, otherwise at the top.
- The header shows title, project, worktree, tool, date, message count and
  session ID. A scrollbar and "message 4/12 35%" show the position.
- `/` searches inside the transcript; matches are highlighted and `n` `N` move
  between them.

### Copying

- `y` copies the session ID and `Y` or `Ctrl+Y` copies a resume command that
  starts in the session's folder: `cd '/path/to/project' && claude --resume <id>`.
- recall stays open and the bottom row confirms what was copied.
- Without a system clipboard (e.g. over SSH) it uses the OSC 52 terminal escape
  code.

### Screen

- The bottom row shows the mode (`NORMAL`, `PREVIEW`, `SEARCH`) and the main
  keys for the current view; keys that don't fit are left out, `? help` stays.
- The `?` panel lists every key in up to three columns of even height, and
  scrolls with `j` `k` when the terminal is too small.

## Outside recall

A launchd job deletes claude-mem observer session files older than 7 days, on
both Macs. claude-mem keeps its memory in `~/.claude-mem/claude-mem.db` and
never reads these files again.

- Script: `~/.local/bin/delete-old-claude-mem-observer-sessions.sh`
- Job: `~/Library/LaunchAgents/dev.samg.delete-old-claude-mem-observer-sessions.plist`
  (daily at 04:15 and at login)
- Log: `~/.local/log/delete-old-claude-mem-observer-sessions.log`

## Code

New files:

- `src/config.rs`: reads `~/.config/recall/config.toml`
- `src/project.rs`: project folder and worktree name from a session's folder
- `src/transcript.rs`: transcript view position, search and keys
- `src/time.rs`: reading typed times and `since:`/`until:` search words
- `src/clipboard.rs`: system clipboard with OSC 52 fallback

## Commits

- `6a0d177` Project scope with worktrees, word-start search, paging keys, shortcuts panel
- `3976f1f` Transcript view, titles, worktree labels, tool and date filters, copy keys
- `88fe6ec` Bottom row shows only status messages and how to open the shortcuts panel
- `65ea099` Transcript view uses the full terminal width
- `596427b` nvim-style normal and search modes, list/preview panes, key hints per view
- `0bdab81` Only Ctrl+C quits; only Ctrl+R resumes from the transcript view
