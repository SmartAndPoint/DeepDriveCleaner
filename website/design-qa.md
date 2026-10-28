# Two-site implementation QA — 28 October 2026

Final result: passed

## Evidence and state

Source visual truth:
- Product: `/Users/ekonev/.codex/generated_images/01a18175-42f1-7942-bbaa-45757d96fd22/exec-1e95af72-631d-4942-a36b-73063fb723fd.png` (1487 × 1058).
- CLI: `/Users/ekonev/.codex/generated_images/01a18175-42f1-7942-bbaa-45757d96fd22/exec-93c5a5c1-0cae-47a3-9fd6-1ce7a70de787.png` (1003 × 1568).

Rendered implementation: `http://127.0.0.1:4175/` and `http://127.0.0.1:4176/`. Browser screenshots in `qa/{product,cli,development}-{en,zh}-{1440,390}.jpg` cover all twelve page/language/viewport combinations. Desktop CSS viewport 1440 × 1024; mobile 390 × 844. Captures are respectively 1440 × 1024 and 390 × 844 pixels. In-app capture scaling leaves padding at density 1.2: `qa/compare.html` displays implementation images at 120% and clips the padding. Screenshot pixels are unchanged. Do not infer layout overflow from that padding; DOM geometry was checked independently.

Full-view evidence: `qa/design-comparison.jpg` places both selected references alongside both desktop implementations in the same image. Focused copy/controls/risk details were inspected in the actual desktop and mobile screenshots, including `qa/product-interactions.jpg`. References are design directions rather than exact content states: approved updated copy, GUI-specific steps, standalone CLI availability and a unified human/agent workflow intentionally supersede the mock text. English/macOS selected for comparison; Chinese and unavailable platforms checked separately.

## Findings and fidelity surfaces

No remaining actionable P0/P1/P2 findings.

- Typography: system sans headings and body, monospace commands; both language hierarchies and small labels remain legible. Different wrapping from the mock follows approved headline/content changes. Chinese uses system CJK fallbacks.
- Spacing/layout: light two-column product cards, laptop hero, restrained dividers; CLI retains yellow rule and field-guide rhythm. Mobile stacks cards and commands. Added instructions make the pages longer than the mock, intentionally.
- Colors/tokens: pale blue product background and blue actions; warm white/yellow CLI accents; shared muted risk colors, visible focus outlines.
- Assets: real app icon, generated localized laptop illustrations, licensed Heroicons. Hero is explicitly labelled illustration, not an app screenshot. Sharp raster art and vector interface icons preserve the selected direction.
- Copy/content: app and CLI clearly one product; no Windows/Linux GUI claim; unpublished CLI builds labelled planned. EN/中文 installation, agent boundaries, examples, legal terms and contribution links are consistent. Sample data is labelled. Claude Artifact link preserved.

## Comparison history and fixes

1. Earlier mobile P2: identity cross-links overflowed by 16 px because width 100% was combined with a left margin. Fixed `.identity-links` to `width: calc(100% - 32px)`. Recaptured the twelve evidence states: document scroll width equals viewport width, no right-overflow elements. Rechecked desktop reference/implementation composition after the fix.
2. Runtime startup failure found in initial browser testing: URL parameters were read before initialization. Moved initialization before preference resolution; current pages render correctly. Fresh browser error log is empty. This was a runtime repair rather than a visual fidelity iteration.
3. Static/reference review corrected development assets on localized routes, a duplicated translation key and missing shipped scheduled-scan description. Final static and runtime checks pass.

## Interactions and verification

- Browser: cross-site links retain language/OS; Chinese route and text update; Windows state hides downloads and five CLI command blocks; app remains clearly macOS-only.
- Browser: install copy produces “Copied to clipboard”; selecting review risk updates explanation and selection; platform FAQ expands with correct availability text. Current browser error log: none.
- Independent reviewer: twelve static HTML documents, assets, local anchors, canonical/alternate links and translation parity pass. Seven runtime scenarios cover denied storage, URL/path/storage precedence, cross-origin state/anchor retention, unavailable-command copy guard, clipboard selection/focus fallback, and delayed clipboard success/failure after locale/platform changes.
- No scanner or cleanup is invoked by the website. Live cleanup is outside website QA.

## Checklist

- [x] Compare selected visual references and rendered pages together.
- [x] Check five fidelity surfaces and responsive states.
- [x] Fix and recapture mobile overflow.
- [x] Verify primary browser interactions and current errors.
- [x] Independent source review and appropriate static/runtime checks.
- [ ] Deploy both exact reviewed sources and validate custom DNS separately.

Residual limitation: no physical Windows/Linux browser was used; current support notices reflect release assets, not a tested binary on those platforms. DNS and publication are operational follow-up, not a visual QA finding.
