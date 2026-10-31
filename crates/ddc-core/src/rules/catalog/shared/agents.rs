//! Shared rules: the coding agents' harness data (ADR "Agent footprint:
//! worktrees, harness data and bounded consent for agents", §3). Every
//! harness keeps it in the same home-relative folders on every OS.

use super::super::rule;
use crate::class::Category::*;
use crate::class::RiskClass::*;
use crate::class::{Removal, RiskClass};
use crate::model::{Origin, OriginHarness, OriginKind};
use crate::rules::{Matcher, Rule};

/// A rule of a coding agent's harness. Its `review` rules are `listed_only`:
/// only a person removes them, never `--yes` (ADR "Agent footprint:
/// worktrees, harness data and bounded consent for agents", §3).
const fn agent(
    id: &'static str,
    class: RiskClass,
    removal: Removal,
    matcher: Matcher,
    harness: OriginHarness,
    kind: OriginKind,
) -> Rule {
    Rule {
        origin: Some(Origin { harness, kind }),
        listed_only: matches!(class, RiskClass::Review),
        ..rule(id, AppCache, class, removal, matcher)
    }
}

// No owner by process name: §5's in-use check stays, and an owner match
// would hold back every path whenever Claude Code runs.
pub(crate) const AGENT_CLAUDE_CODE_SCRATCH: Rule = Rule {
    hint: "debug logs and legacy folders Claude Code no longer writes",
    ..agent(
        "agent.claude-code-scratch",
        Safe,
        Removal::Delete,
        Matcher::KnownPaths(&[".claude/debug", ".claude/todos", ".claude/statsig", ".claude/logs"]),
        OriginHarness::ClaudeCode,
        OriginKind::RuntimeCache,
    )
};

// Sessions are the entries of these two (story "Harness data rules"); the
// folders themselves have no method, and each hint speaks of one session.
pub(crate) const AGENT_CLAUDE_CODE_FILE_HISTORY: Rule = Rule {
    hint: "Checkpoints of this session. /rewind cannot use them once moved.",
    ..agent(
        "agent.claude-code-file-history",
        Review,
        Removal::None,
        Matcher::KnownPaths(&[".claude/file-history"]),
        OriginHarness::ClaudeCode,
        OriginKind::SessionData,
    )
};

// One finding per project folder: no rule runs inside it, so its
// `memory/` (auto memory) is never another rule's finding.
pub(crate) const AGENT_CLAUDE_CODE_TRANSCRIPTS: Rule = Rule {
    hint: "This session's transcript. --resume of it fails until it is put back from the Trash, before it is resumed.",
    ..agent(
        "agent.claude-code-transcripts",
        Review,
        Removal::None,
        Matcher::KnownChildren(&[".claude/projects"]),
        OriginHarness::ClaudeCode,
        OriginKind::SessionData,
    )
};

pub(crate) const AGENT_CLAUDE_CODE_HISTORY: Rule = Rule {
    hint: "A recalled prompt loses its pasted text; past /insights reports are gone.",
    ..agent(
        "agent.claude-code-history",
        Review,
        Removal::Trash,
        Matcher::KnownPaths(&[".claude/paste-cache", ".claude/usage-data"]),
        OriginHarness::ClaudeCode,
        OriginKind::SessionData,
    )
};

// Codex, opencode and Cursor go to the Trash only while their harness is
// not live: the guard refuses a running one, or a held Codex lock, as
// `agent-live` (story "Harness data rules").
pub(crate) const AGENT_CODEX_SESSIONS: Rule = Rule {
    hint: "Codex's past threads; Codex cannot reopen them once moved. Quit Codex, and its app-server, to move them.",
    ..agent(
        "agent.codex-sessions",
        Review,
        Removal::Trash,
        Matcher::KnownPaths(&[".codex/sessions", ".codex/log"]),
        OriginHarness::Codex,
        OriginKind::SessionData,
    )
};

pub(crate) const AGENT_CODEX_RUNTIMES: Rule = Rule {
    hint: "Downloaded runtimes; Codex fetches them again on demand. Quit Codex to move them.",
    ..agent(
        "agent.codex-runtimes",
        Review,
        Removal::Trash,
        Matcher::KnownPaths(&[".cache/codex-runtimes"]),
        OriginHarness::Codex,
        OriginKind::RuntimeCache,
    )
};

// By subfolder, never whole: `.local/share/opencode` also holds
// `auth.json` (provider credentials) and `opencode.db`, its live index.
pub(crate) const AGENT_OPENCODE_DATA: Rule = Rule {
    hint: "opencode's stored sessions, snapshots and logs; opencode cannot show those sessions once moved. Quit opencode to move them.",
    ..agent(
        "agent.opencode-data",
        Review,
        Removal::Trash,
        Matcher::KnownPaths(&[
            ".local/share/opencode/storage",
            ".local/share/opencode/snapshot",
            ".local/share/opencode/log",
            ".cache/opencode",
        ]),
        OriginHarness::Opencode,
        OriginKind::SessionData,
    )
};

pub(crate) const AGENT_CURSOR_AGENT_DATA: Rule = Rule {
    hint: "Cursor's agent transcripts per project, and its AI-code tracking; Cursor cannot show them once moved. Quit Cursor to move them.",
    ..agent(
        "agent.cursor-agent-data",
        Review,
        Removal::Trash,
        Matcher::KnownPaths(&[".cursor/projects", ".cursor/ai-tracking"]),
        OriginHarness::Cursor,
        OriginKind::SessionData,
    )
};

// Sealed, like app bundles: never a finding, and no rule fires inside.
// The live session records; the shell snapshots a live session sources
// before every Bash command without holding them open; what the person
// installed; the person's plans and chats. `.cursor/worktrees` and
// `.codex/worktrees` are not sealed: the rules work inside a worktree.
pub(crate) const AGENT_CLAUDE_CODE_SEALED: Rule = Rule {
    hint: "Claude Code's live session records, shell snapshots and installed plugins",
    listed: false,
    ..rule(
        "agent.claude-code-sealed",
        Personal,
        UserData,
        Removal::None,
        Matcher::KnownPaths(&[".claude/sessions", ".claude/shell-snapshots", ".claude/plugins"]),
    )
};

pub(crate) const AGENT_CURSOR_SEALED: Rule = Rule {
    hint: "Cursor's installed extensions and plugins, and the person's plans and chats",
    listed: false,
    ..rule(
        "agent.cursor-sealed",
        Personal,
        UserData,
        Removal::None,
        Matcher::KnownPaths(&[".cursor/extensions", ".cursor/plugins", ".cursor/plans", ".cursor/chats"]),
    )
};
