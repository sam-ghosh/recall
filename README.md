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

**That's it.** Start typing to search. Enter to jump back in.

| Key | Action |
|-----|--------|
| `↑↓` | Navigate sessions |
| `Pg↑/↓` | Page through sessions |
| `Ctrl+U` / `Ctrl+D` | Half a page up/down |
| `Home` / `End` | First/last session |
| `Shift+↑↓` | Previous/next message in the preview |
| `Ctrl+E` | Expand message |
| `Ctrl+A` | Cursor to start of search |
| `Enter` | Resume conversation |
| `Tab` | Copy session ID |
| `/` | Toggle scope (project/everywhere) |
| `?` / `F1` | Keyboard shortcuts panel (`?` when the search is empty) |
| `Esc` | Clear search; quit when empty |

Typed words match the start of words: `xeni` finds `xenia`. Every word must match.
Quotes search for an exact phrase: `"gunicorn restart"`.

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
