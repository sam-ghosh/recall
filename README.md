# recall&nbsp;&nbsp;&nbsp;[![Mentioned in Awesome Claude Code](https://awesome.re/mentioned-badge.svg)](https://github.com/hesreallyhim/awesome-claude-code)

Search and resume your Claude Code conversations. Also supports Codex, OpenCode and Factory (Droid).

**Tip**: Don't like reading? Tell your agent to use `recall search --help` and it'll search for you.

![screenshot](screenshot-dark.png)

## Install

**Homebrew** (macOS/Linux):
```bash
brew install zippoxer/tap/recall
```

**WinGet** (Windows):
```bash
winget install zippoxer.recall
```

**Cargo**:
```bash
cargo install --git https://github.com/zippoxer/recall
```

**Binary**: Download from [Releases](https://github.com/zippoxer/recall/releases)

## Use

Run:
```bash
recall
```

**That's it.** Start typing to search. Enter opens the conversation, Enter again jumps back in.

### Session list

| Key | Action |
|-----|--------|
| `↑↓` | Navigate sessions |
| `Pg↑/↓` | Page through sessions |
| `Ctrl+U` / `Ctrl+D` | Half a page up/down |
| `Home` / `End` | First/last session |
| `Shift+↑↓` | Previous/next message in the preview |
| `Ctrl+E` | Expand message |
| `Ctrl+A` | Cursor to start of search |
| `Enter` | Open the transcript view |
| `Ctrl+R` | Resume conversation |
| `Tab` | Copy session ID |
| `Ctrl+Y` | Copy resume command (`cd '<folder>' && claude --resume <id>`) |
| `/` | Toggle scope (project/everywhere) |
| `Ctrl+S` | Filter by tool: all, Claude, Codex, Factory, OpenCode |
| `?` / `F1` | Keyboard shortcuts panel (`?` when the search is empty) |
| `Esc` | Clear search; quit when empty |

### Transcript view

The whole conversation, full screen, with vim-style keys.

| Key | Action |
|-----|--------|
| `j` `k` / `↑↓` | Scroll one line |
| `d` `u` / `Ctrl+D` `Ctrl+U` | Half a page down/up |
| `f` `b` / `Ctrl+F` `Ctrl+B` / `Space` / `PgDn` `PgUp` | Full page down/up |
| `g` `G` / `Home` `End` | Top/bottom |
| `]` `[` / `J` `K` / `Shift+↓↑` | Next/previous message |
| `}` `{` | Next/previous message of yours |
| `/` then `n` `N` | Search in the transcript, next/previous match |
| `Enter` / `Ctrl+R` | Resume conversation |
| `y` / `Tab` | Copy session ID |
| `Y` / `Ctrl+Y` | Copy resume command |
| `q` / `Esc` | Back to the session list |

Typed words match the start of words: `xeni` finds `xenia`. Every word must match.
Quotes search for an exact phrase: `"gunicorn restart"`.
Date words limit sessions by when they were last active: `since:2w`, `after:2025-12-01`,
`until:yesterday`, `before:3d` (units `m` `h` `d` `w` `mo` `y`, or `today`, `yesterday`, a date).

The list shows each conversation's title (Claude Code `/rename` or its generated
title, Codex thread name) and `⎇ <worktree>` for sessions in a git worktree.

The project scope shows sessions started in the launch folder, in folders inside
it, and in its git worktrees (`<project>__worktrees/<branch>` or
`<project>/.worktrees/<branch>`). `recall search --cwd` and `recall list --cwd`
use the same rule.

## Ask it to Search for You
Simply tell your agent:
```
use `recall search --help`
```

Example:
```
pls find me the last conversation where we deployed to staging, use `recall search --help`
```

## MCP
No MCP required. The `recall search` CLI fulfills the same purpose. See [Ask it to Search for You](#ask-it-to-search-for-you).

## Customize

### Skip session files

Session files whose path contains any string in `skip_paths_containing` are not
indexed. The default skips claude-mem's background observer sessions. To change
it, create `~/.config/recall/config.toml`:
```toml
skip_paths_containing = ["claude-mem-observer-sessions", "some-scratch-project"]
```

### Skip automated sessions

Sessions whose first message starts with any string in
`skip_sessions_starting_with` are not indexed, e.g. scheduled runs. After a
change, recall indexes every session again on its next start.
```toml
skip_sessions_starting_with = ["[IMPORTANT: You are running as a scheduled cron job"]
```

### Resume commands

recall's resume commands can be configured with environment variables.

For example, to resume conversations in YOLO mode, add this to your `.bashrc` or `.zshrc`:
```bash
export RECALL_CLAUDE_CMD="claude --dangerously-skip-permissions --resume {id}"
export RECALL_CODEX_CMD="codex --dangerously-bypass-approvals-and-sandbox resume {id}"
```

---

![light mode](screenshot-light.png)

---

Made with ❤️ by [zippoxer](https://github.com/zippoxer) and Claude.
