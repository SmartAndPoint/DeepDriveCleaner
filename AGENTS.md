<!-- projectstore:agents v5 (managed by projectstore — edit outside markers) -->
## projectstore agents

- **A feature-sized request opens a vault artifact before it opens an editor.**
  Analysis → placement (which epic, which story) → an ADR and/or spec when the
  "how" is non-trivial → `projectstore:critic` → only then implementation →
  `projectstore:reviewer`. "Feature-sized" is not a judgement about how the
  request was phrased — it is about what the work touches: if you are about to
  write across several source files, open the story first.
- **Report instruction conflicts; do not arbitrate them.** If a session-level or
  harness-level instruction contradicts this block, say so and ask which wins.
  Resolving it silently is how the contradiction becomes invisible to the person
  who could have settled it.
- When spawning any agent below, resolve its model and its effort from
  `.projectstore/harness/<harness>.json` → `agents.per_agent.<name>.model ?? agents.default.model`
  and `agents.per_agent.<name>.effort ?? agents.default.effort`, where `<name>` is
  the **bare** agent name (`critic` for `projectstore:critic`), and pass them as
  the spawn's model and effort parameters, the effort only when the spawn offers
  one. No key — pass nothing.
- After authoring or revising any vault artifact (ADR/research/epic/story) or
  design proposal: run the `projectstore:critic` agent on it before treating it final.
- Before implementing an epic/story: consult `projectstore:planner` — it plans
  against how prior epics map to the codebase (`code_refs`).
- After writing code, before commit / story-done: run `projectstore:reviewer` —
  it verifies the diff actually closes the story's acceptance criteria.
- When discussing vault contents, reference artifacts by their frontmatter
  `title:` (with their parent epic), never by session-invented shorthand.
<!-- /projectstore:agents -->

## Every change goes through a pull request

- `main` takes no direct push, here or in the vault: a ruleset requires a pull request
  and refuses `git push origin HEAD:main`. This binds every session and every agent,
  whatever it changes: code, docs, the vault, one line.
- A pull request that merges without conflicts may be merged by the session that opened
  it, without asking. One with conflicts is rebased onto `origin/main` first; never
  force it through, and never ask for a ruleset bypass.
- Here, start each branch from a fresh `origin/main` (`git fetch && git switch -c
  <branch> origin/main`), push it (`git push -u origin <branch>`, gated by the hook),
  then `gh pr create --fill` and `gh pr merge --auto --rebase`. It merges once the
  Gate's `scope`, `gate`, `coverage`, `app` and `release-build` pass.
- The vault is one checkout on `main`, shared by every session, with other sessions'
  uncommitted edits in it: never switch its branch, never `reset --hard` it, and never
  `git pull --rebase --autostash` in it. Commit on `main`, push to a topic branch, open
  and merge its PR, then move local `main` onto the merged commits with `update-ref`: the
  steps are in the vault's own `AGENTS.md`.
- A step is done when its PR is merged, not when auto-merge is on: a red check leaves it
  waiting, and a conflict with a newer `main` can stop it, with no more than an email.
  Wait with `gh pr checks <n> --watch --fail-fast`, then check that
  `gh pr view <n> --json state` says `MERGED`. On a conflict, `git fetch && git rebase
  origin/main && git push --force-with-lease`.
- Never pass `--delete-branch` to `gh pr merge`: on a PR that merges at once it also
  deletes the local branch, which a session may still be standing in. GitHub deletes the remote branch.
- A rebase merge gives the commits new SHAs: the branch's green run never counts for a
  release, `main`'s own run after the merge does. To release, merge the bump's PR, then
  `git switch main && git pull --ff-only`, then `make tag`.

## The gate

- Run `make tools` and `make hooks` once per clone. The hooks run `make lint` (fmt and
  clippy) before every commit and `make gate-fast` (the unit tests, no load trees)
  before every push. CI runs `gate-fast` on a branch and the whole `make gate` on
  main, which a release needs. For code work the unit tier is the bar; the load tests
  are for a release (the owner, 2026-10-31).
- Never commit or push with `--no-verify`: fix what the gate names.
- The pre-push tier takes about a minute after a core change and longer cold: run
  `git commit` and `git push` with a 10-minute tool timeout.
- A commit or push whose every path is on `.config/rust-free-paths` (the app's own
  sources, `website/`, `docs/releases/`) skips the Rust tiers, locally and in CI; a
  push that touches `app/Packages/` runs `make ddckit-test` instead. A green hook on
  such a push says nothing about Rust. `make tag` and `make dist` need a Gate run that
  ran the Rust jobs on the release commit itself: put the release notes in the commit
  that bumps the version, or start one by hand with `gh workflow run gate.yml --ref main`.

## Versions and releases

- More than one session releases. The version lives in one place: `version` under
  `[workspace.package]` in `Cargo.toml`. `make tag`, `make dist` and `release.yml` read it;
  the app's `MARKETING_VERSION` must equal it and its `CURRENT_PROJECT_VERSION` (the build
  Sparkle compares) must grow past the last tag's. Change them only with
  `make bump V=major|minor|patch|X.Y.Z`, never by hand: it writes `Cargo.toml`, both lock
  files and the app's version and build together, and lists what merged since the last tag.
- Bump from `origin/main` as it is now, never from the version you remember: start the
  bump's branch fresh. Pick the part by everything merged since the last tag, every
  session's work: a new capability is minor, fixes alone are patch; no major while 0.x.
  `docs/releases/X.Y.Z.md` covers all of it, in the bump's commit.
- A conflict on the version files means another session bumped first: rebase onto
  `origin/main`, read its version, and decide again; its tag may already be out.
- Run `make tag`, `make dist` and `scripts/publish-release.sh` back to back. If `main`
  moves before the publish, the script refuses: leave the unpublished tag and release the
  next patch (0.11.0 and 0.12.0 went out as 0.11.1 and 0.12.1, the owner, 2026-10-31).
