# DeepDriveCleaner — Product Hunt launch copy

Draft for owner review, 2026-10-28. No listing, comment, outreach, audience change, or launch has been published from this document.

Astra reviewed the latest visual mock and launch copy. Verdict: positioning fits a Product Hunt launch; complete the access, real-product assets, eligibility, and approval-wording changes below before launch. This is not a completed launch-readiness sign-off.

## Positioning

Lead with one useful product: a native Mac disk cleanup app and a CLI that developers and their agents can use. Show the app and CLI together. Explain JSON reports, cleanup previews, and checks before cleanup as concrete capabilities.

The proposed path is: try DeepDriveCleaner → explore its GitHub repository → contribute a scoped improvement → inspect the ProjectStore workflow behind it. This is a launch hypothesis, not a prediction of traffic or stars.

Give SmartAndPoint and the maker visible credit. Use the development page and maker comment to show how people and agents collaborate through ProjectStore, with links to real decisions and stories.

The additional campaign angle is the disk footprint of coding-agent work: repeated dependency environments, build outputs, and caches. This is a concrete use case for the product, not a claim that every finding was created by an agent. Do not advertise cleanup of orphaned agent worktrees or arbitrary temporary directories until that behavior is verified in a released build.

Campaign line:

> Your agents build. You review the cleanup.

Supporting copy:

> Find caches, dependency folders and build outputs from your development work, including work done with agents. Inspect the findings and preview a cleanup plan before deciding what to remove.

Simplified Chinese campaign line:

> 智能体负责构建，你来审阅清理计划。

> 查找开发工作（包括智能体参与的开发）留下的缓存、依赖目录和构建产物。先查看扫描结果、预览清理计划，再决定删除哪些内容。

Keep the broad Product Hunt tagline below. Test this narrower campaign angle in the maker story and product demonstration before replacing the entire product identity with agent-specific cleanup claims.

## Product Hunt fields

Name:

> DeepDriveCleaner

Tagline — 50 characters:

> Fast Mac disk cleanup for developers and AI agents

Description — 206 characters:

> Find caches, dependencies and build outputs with a native Mac app and a CLI for Claude Code, Codex and other agents. Inspect JSON reports, preview a cleanup plan, and re-check selected items before cleanup.

Use the canonical product URL in the submission. The official preparation guide says shortened and tracking links are not accepted in that field.

## Website hero and actions

Headline:

> Fast disk cleanup for developers and agents.

Supporting line:

> A native app for you. A CLI for your agent. One engine.

Description:

> Find caches, dependencies, and build outputs. Review a cleanup plan, then clean the items you approve.

Keep installation as the primary action. Show the current requirement beside it: **macOS 26 · Apple Silicon · App + CLI**. Windows and Linux have no release available; the selector must say so.

Beside installation, link **Source-available · PolyForm Noncommercial**. On a page for Product Hunt traffic, keep the current Mac release prominent and put other-platform availability in a secondary position. Replace the old mock's “The engineering field guide” label with **Disk cleanup, your way**. The Product Hunt-specific hero can say **Fast Mac disk cleanup for developers and agents.**

Two usage routes:

- **Use the app** — See findings. Review your selection. Confirm cleanup.
- **Use with an agent** — Claude Code, Codex, or another CLI agent. Ask your agent to show the cleanup plan and wait for your approval.

Short supporting line:

> JSON reports · Dry-run plans · Re-checks before cleanup

Near cleanup instructions:

> Cleaning deletes files. Review the plan and confirm your selection.

The CLI supports non-interactive operations; do not claim that it always prompts. A report is not deletion consent. Checks reduce risk and do not guarantee that an intended deletion is a good decision.

Secondary repository action:

> View on GitHub

Optional supporting invitation on the repository/contribution section:

> If DeepDriveCleaner is useful to you, star it on GitHub.

Contribution heading:

> Contribute with your agent.

Contribution text:

> Bring Claude Code, Codex, or your preferred agent. Read the shared project context, agree on a scoped change, and follow the contribution runbook.

Link the original contribution terms beside this invitation. The code is source-available under PolyForm Noncommercial 1.0.0; do not label it OSI open source.

Add one or two verified, scoped starter issues once suitable public tasks exist. Do not invent issue links, labels, or promises about contribution readiness.

Maker credit:

> Built by Evgenii Konev at SmartAndPoint.

Confirm the maker's preferred public spelling and profile links before posting. Do not invent social handles.

## Website copy in Simplified Chinese

Headline:

> 为开发者和 AI 智能体打造的快速磁盘清理工具。

Supporting line:

> 你用原生应用，智能体用命令行，共用同一套清理引擎。

Description:

> 查找缓存、依赖和构建产物。先查看清理计划，再清理你确认的项目。

Usage routes: **使用应用** / **通过智能体使用**.

Contribution heading: **与你的智能体一起贡献代码。**

## Maker comment — proposed English text

Hi Product Hunt — I'm Evgenii from SmartAndPoint.

DeepDriveCleaner finds the caches, dependency folders and build outputs that development leaves on a Mac. Its native app and CLI use the same engine: review findings in a window, or let Claude Code, Codex or another CLI agent inspect JSON reports and preview a cleanup plan.

Coding agents can create more dependency environments, build outputs, and caches as they work. DeepDriveCleaner helps you inspect those development leftovers and choose what to clean.

Selected items are checked again before cleanup. Cleaning deletes files, so review the selection before authorizing it.

Version 0.3.0 supports macOS 26 on Apple Silicon. The source is available under PolyForm Noncommercial.

DeepDriveCleaner is also a worked example of developing with ProjectStore. Its development vault records decisions, stories and review evidence. If you want to contribute with your own agent, start with the contribution runbook.

I'd value feedback on what the scan finds, what it misses, and whether the cleanup preview gives you enough information to decide.

## Gallery and demonstration

Prepare a small sequence grounded in the actual released product:

1. **App + CLI, one engine.** A real app screenshot next to a real terminal report, making both usage routes immediately visible.
2. **Understand the findings.** Show actual risk classes and selection behavior. Label any invented or anonymized example data.
3. **Inspect → preview → approved cleanup.** Show the read-only JSON command and a dry run, plus the app's review/confirmation and checks. No autonomous-cleanup claim.
4. **Contribute with your agent.** Show a real ProjectStore decision/story and the short contributor entry point. Keep this after the product demonstration.

The generated website mock is a design reference. Its illustrative app preview is not a real app screenshot for the launch gallery.

## Links to prepare

- Product installation/release: https://github.com/SmartAndPoint/DeepDriveCleaner/releases
- Code: https://github.com/SmartAndPoint/DeepDriveCleaner
- Contribution runbook and terms: https://github.com/SmartAndPoint/DeepDriveCleaner#contributing
- Shared project vault: https://github.com/SmartAndPoint/DeepDriveCleaner-projectstore
- ProjectStore: https://projectstore.dev
- Design reference: https://claude.ai/artifact/1mdoyc7asMbj2qbtkDG2PV
- SmartAndPoint: https://github.com/SmartAndPoint

## Signals to examine

Compare the period before and after launch: visits/referrals where available, outbound release/repository clicks if analytics already exist, GitHub star changes, substantive issues, and first contributions from new external authors. Ask users whether they completed a first scan; do not imply that a download or click proves installation or use.

GitHub star changes alone cannot establish Product Hunt attribution. No new telemetry or tracking is introduced by this document.

## How the tool reaches an agent

Treat the following as distribution hypotheses to validate, not as a measured ranking of acquisition channels. A person usually chooses which tools an agent may use; agent discovery is not independent purchasing intent.

1. **A person configures their agent.** Prepare an official, reviewable skill with the canonical SmartAndPoint repository as its source, then evaluate relevant plugin/skill marketplaces. Provide client-specific installation instructions only after testing them. A registry presence can help establish provenance; it cannot stop similarly named third-party packages.
2. **A person or agent searches for disk help.** Publish accessible explanations of developer disk usage, System Data, caches, app and CLI installation, JSON reports, and cleanup decisions. Keep the short agent reference and schemas linked from normal pages. These make the product understandable and discoverable; neither an `llms.txt` file nor a listing guarantees that a model recommends it.
3. **A coding task hits ENOSPC.** Provide a concise diagnostic entry: identify the affected volume and available resources, inspect supported findings, show a cleanup preview, and ask the person to approve a selection. ENOSPC can have causes other than removable caches; a failed task must not become authorization to delete files or download and execute an unverified tool.

For launch sequencing, establish the canonical repository and documentation first, prepare the official skill as a distinct implementation task, then test selected marketplaces. Add an MCP adapter only if a real use case warrants one; the current CLI is not itself an MCP server.

Moltbook is a low-priority experiment, not an acquisition forecast. Research about account impersonation does not establish a conversion rate. Any future participation should disclose human/company authorship and avoid automated promotional posting. No posts or messages are authorized by this strategy document.

## Security claims and their boundaries

The proposed agent workflow is **inspect → explain → preview → obtain the person's approval → act on that selection**. Instructions to wait for approval do not constitute a technically enforced approval boundary.

| Evidence in the current product | Accurate copy boundary |
| --- | --- |
| Risk classes, protected paths, and checks before actions | Explain the checks and their refusal behavior. They reduce some filesystem cleanup risks; they do not determine whether an agent's requested action matches the person's intent. |
| `clean --items` consumes an explicit list and checks its entries | The CLI does not authenticate a human approval for that list and does not prompt. Do not say that all deletion is possible only through a human-confirmed list; other clean modes exist. |
| `clean --yes` skips interactive questions within its supported scope | Avoid “always asks,” “no unattended execution,” or equivalent universal promises. |
| Scheduled/background scans have no cleanup path | “Scheduled scans do not clean files” is supported. This is narrower than claiming that the whole CLI cannot be automated. |
| Trash actions and an action journal | Describe which actions use the Trash. Permanent deletion and owning-tool cleanup also exist. A journal is not a backup or an undo guarantee. |

These controls do not establish prompt-injection resistance, prevent credential theft by a malicious skill, or sandbox an agent that can run other shell commands. The public story should be inspectable cleanup behavior and explicit workflow choices, without an untested security guarantee.

A proposed official skill should identify its source and version, use read-only inspection and preview by default, request only necessary permissions, treat files/output as data rather than instructions, and tell the agent to obtain approval for a specific selection. Avoid unverified remote scripts, permission escalation, or deletion merely because a task encountered ENOSPC. Packaging, adversarial tests, publication, and any stronger enforcement mechanism require their own implementation/review; none is delivered here.

## Measurement story from the owner

On 2026-10-28 the owner reported observing approximately **790,000 folders in one `$TMPDIR`**, attributed in that investigation to abandoned agent activity. This is a user-supplied observation from one machine, not a DDC benchmark or a verified general statistic.

Before using it as a public case study, retain the counting method, machine/environment context, folder attribution evidence, definition of “abandoned,” and aggregate results. Establish count, occupied space, reclaimable space, and supported cleanup behavior separately. Do not infer that all 790,000 folders are safe to remove, that DDC currently recognizes them, or that this is typical of other machines. Publish sanitized evidence without private paths.

## Fact-check of the supplied ecosystem note

- **OpenClaw Foundation:** its official announcement is dated **2026-07-08**. Do not present the formal foundation announcement as having happened in February. [Official announcement](https://openclaw.ai/blog/introducing-openclaw-foundation)
- **ClawHavoc:** the 341 figure belongs to an earlier audit of 2,857 skills. A later OpenClaw/NVIDIA paper cites that historical result and an update to 824; these are dated observations, not a current count or a measure of DDC's protection. The original Koi article now redirects, so avoid treating its current URL as the original readable report. [OpenClaw/NVIDIA paper, section 2.3](https://openclaw.ai/publications/clawhub-security-signals.pdf)
- **Moltbook:** Wiz documented humans posting as agents and account impersonation during its February 2026 review. That supports caution about authorship and participation metrics; it does not show that a promotional channel has zero conversion. [Wiz research](https://www.wiz.io/blog/exposed-moltbook-database-reveals-millions-of-api-keys)

The note's broad claims that agents almost never install tools independently, or that model recommendations replace SEO, remain strategic hypotheses. Search visibility, trustworthy distribution, and model-readable documentation can complement one another.

## Readiness

Copy readiness is separate from launch readiness. The existing Sites website was last verified owner-private. Public Product Hunt visitors need an accessible, finalized product page; changing that audience is a separate owner action. Before launch, verify the release requirements and install path, replace mock app art with real product captures, check the EN/Chinese site, and confirm maker/profile attribution.

Ask Product Hunt visitors for feedback and conversation. The platform prohibits direct requests for Product Hunt upvotes. A GitHub star invitation is a distinct secondary repository action.

## Official sources checked 2026-10-28

- [Product Hunt Launch Guide](https://help.producthunt.com/en/articles/17350772-launch-guide): tagline up to 60 characters, description up to 260, personal maker identity, product visuals, and a maker comment.
- [Preparing for launch](https://www.producthunt.com/launch/preparing-for-launch): plain product language, canonical submission URL, maker story, and feedback.
- [Sharing your launch](https://www.producthunt.com/launch/sharing-your-launch): authentic engagement and the rule against asking for Product Hunt upvotes.

Product facts were checked against the local README, DISCLAIMER, license, and website agent reference. This is a copy and positioning review; no live Product Hunt listing or conversion funnel has been audited.
