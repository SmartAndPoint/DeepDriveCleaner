# Social link preview QA — 28 October 2026

Final result: passed

Scope: the social-preview completion of “English and Chinese product website with development runbook” under “Product website and agent-first contribution guide”. The full story remains in progress until its separate domain/redirect work is complete.

## Artwork

Four opaque RGB PNGs are provided under `src/assets/`: `social-product-en-v2.png`, `social-product-zh-v2.png`, `social-cli-en-v2.png`, and `social-cli-zh-v2.png`. All are 1731 × 909, approximately 1.91:1 and below 5 MiB. Open Graph width/height match the encoded files. Original generation/edit prompts are in `design-assets/social-preview-prompts.md`.

The owner requested Rust-community copy: “Blazingly fast disk cleanup.” / “For developers & agents.” Chinese images read “极速磁盘清理。” / “为开发者与智能体打造。”. This is qualitative campaign wording, not a measured comparison or a safety guarantee. Names, domains and scan/preview commands stay literal. No visible website headline or other body copy was changed.

All four files were viewed at full size. Browser thumbnail review used 320px CSS image widths at a 1280 × 720 viewport. Exact slogans and commands, icon fidelity, contrast, margins and crop boundaries are readable. Evidence: `/private/tmp/ddc-social-qa/final-v2-thumbnails.jpg`. The preview page merely displays the actual PNGs; it is not a claimed screenshot of any social platform.

## Metadata and runtime

- Twelve raw HTML documents across the two output roots pass checks for unique complete OG/Twitter fields, localized title/description/alt/locale, preserved canonical URLs, absolute HTTPS image URLs, PNG signature/dimensions and matching images in each output.
- Both product surfaces use the public product origin for image hosting. CLI and development canonical mappings are preserved. Versioned locale-specific asset names avoid reusing neutral draft URLs.
- Independent reviewer ran actual `site.js` in 24 DOM-selector VM contexts: all twelve documents with normal and blocked storage, initial render and EN↔ZH roundtrip restoration. Image URLs and alt text follow the locale.
- Resumed approved browser verification passed ten states: EN→ZH→EN for product/CLI/development, plus startup using the saved Chinese preference. No browser console errors. Evidence: `/private/tmp/ddc-social-qa/browser-v2-metadata.json`.
- The earlier browser check was interrupted by an approval-service usage limit; the resumed normal check succeeded. No alternate browser-control path was used.
- `git diff --check` and the production domains build pass. The independent ProjectStore reviewer reports no blocking code findings after two minor body-copy/provenance corrections.

Postdeployment verification is a separate step: fetch both public homepages, localized pages and all four PNG URLs anonymously, confirm image/png and exact image bytes. External social services cache link previews independently; this work cannot guarantee immediate refresh of previously shared links. No social posts or announcements are sent.
