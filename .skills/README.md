# Agent skills (canonical)

This directory is the **single source of truth** for repo agent skills, in the same layout as [`dkorolev/scsh`](https://github.com/dkorolev/scsh): each skill is a folder with `SKILL.md` (YAML frontmatter + markdown body). Edit skills here — the tool-specific paths below are symlinks.

## Tool discovery paths

| Tool | Project path | Notes |
| --- | --- | --- |
| **Canonical** | `.skills/<name>/` | Author here |
| Cursor | `.cursor/skills/` → `.skills` | Also `~/.cursor/skills/` for personal skills |
| Claude Code | `.claude/skills/` → `.skills` | Also `~/.claude/skills/` |
| Codex | `.agents/skills/`, `.codex/skills/` → `.skills` | Repo; also `~/.agents/skills/`, `~/.codex/skills/` |
| OpenCode | `.opencode/skills/` → `.skills` | Also reads `.claude/skills`, `.agents/skills` |

All symlinks point at this directory so one edit updates every host.

## Skills in this repo

| Skill | Purpose |
| --- | --- |
| [packdiff-pr](packdiff-pr/SKILL.md) | Pack a real GitHub pull request into one offline packdiff HTML page, baking the PR's description in as the `PR-DESCRIPTION.md` notes commit and shipping the PR's review discussion beside it as `review.json` (Import JSON) + `discussion.md` (agents and humans) |

## Adding a skill

1. Create `.skills/<skill-name>/SKILL.md` with `name` and `description` frontmatter (name must match the folder).
2. Author only here — never in the symlinked host paths (`.claude/skills/`, `.cursor/skills/`, …).
3. Invoke via your host (`/skill-name`, `$skill-name`, or natural-language trigger per `description`).
