# DeepDriveCleaner websites

One shared, dependency-free source produces two independent English and Simplified Chinese static websites:

- `deepdrivecleaner.app`: native Mac app, installation, example findings and contribution path.
- `ddc.sh`: CLI releases and one terminal/agent workflow, schemas and cleanup boundaries.

`src/` is the source. Never edit generated `dist/`. `src/content.mjs` owns shared release facts; `src/copy.mjs` owns localized copy. `src/pages.mjs`, `src/site.js` and `src/styles.css` render the pages. `legacy/` preserves earlier design studies outside the production navigation.

## Build and preview

Requires Node.js 20+ and Python 3 for the optional local server; no dependency installation.

From the repository root:

```sh
node website/package.mjs --navigation preview --product-origin http://127.0.0.1:4175 --cli-origin http://127.0.0.1:4176
python3 -m http.server 4175 --bind 127.0.0.1 --directory website/dist/product
# In another terminal:
python3 -m http.server 4176 --bind 127.0.0.1 --directory website/dist/cli
```

Each output includes product, CLI and development pages in English and `/zh/`, plus localized agent references, sitemap and robots files. Explicit valid `?lang=en|zh` wins over route and guarded saved preference. `?os=macos|windows|linux` selects CLI availability. Cross-site links retain language, OS and anchors. Clipboard failure selects the command for manual copying. The website cannot scan or change files.

Navigation modes: `companion` (default) uses local companion pages; `preview` and `hosted` use supplied origins; `domains` uses the intended `.app` and `.sh` domains. Canonical metadata always identifies the intended custom domains. Preview both sites using explicit local origins until DNS is active; production uses `domains`.

## Cloudflare Pages

Create two Git-integrated Pages projects using `SmartAndPoint/DeepDriveCleaner`. These are Pages projects, not Workers projects.

| Setting | Product site | CLI site |
| --- | --- | --- |
| Project name | `deepdrivecleaner` | `ddc-cli` |
| Production branch | `main` | `main` |
| Framework preset | None | None |
| Root directory | `website` | `website` |
| Build command | `node package.mjs --navigation domains` | `node package.mjs --navigation domains` |
| Build output directory | `dist/product` | `dist/cli` |
| Environment variable | `NODE_VERSION=22` | `NODE_VERSION=22` |
| Build watch include path (repository relative) | `website/*` | `website/*` |

Both projects build the same source and publish different outputs. A merge into `main` rebuilds production; other allowed branches/PRs get preview deployments. Ensure preview builds are enabled in each Pages project's branch controls. The production build command deliberately points cross-site links to the custom domains; Pages preview URLs will not automatically link to the other PR's preview. Use the local preview command above to test cross-site changes before custom domains are connected.

Only source and assets are committed; `dist/`, `.site-build/` and local `qa/` evidence are ignored. Changes to app/CLI code outside `website/` do not trigger these builds with the watch path above. Updating binaries or a GitHub Release does not automatically update pinned website facts: edit `src/content.mjs` and translations in a PR after verifying the new artifacts.

Attach `deepdrivecleaner.app` to the product Pages project and `ddc.sh` to the CLI Pages project through Custom domains. Follow `DNS.md` for nameservers, TLS checks and the separate `.com` redirect. Do not use the earlier OpenAI Sites DNS records. Cloudflare production is intended to be public without ChatGPT sign-in; verify anonymous access after deployment and before announcing a launch.

Official references: [Git integration](https://developers.cloudflare.com/pages/get-started/git-integration/), [build settings](https://developers.cloudflare.com/pages/configuration/build-configuration/), [watch paths](https://developers.cloudflare.com/pages/configuration/build-watch-paths/), [custom domains](https://developers.cloudflare.com/pages/configuration/custom-domains/).

## Release facts and safety

Release v0.3.0 was rechecked on 28 October 2026. The native app requires macOS 26 on Apple Silicon. The published standalone CLI archive is macOS ARM64. Windows/Linux CLI builds are planned, with no current release assets; those states hide installation/download commands. Recheck GitHub Releases before changing availability.

Examples total 128.4 GiB and are illustrative, not a scan or promised savings. The laptop is a labelled product illustration. Cleanup can permanently delete files. Explicit selections and `--yes` can bypass prompts; the site does not claim enforced human approval, universal Trash/undo, or prompt-injection resistance. Source is available under PolyForm Noncommercial; contributor commercial relicensing terms are linked.

## Earlier private design previews

The retained `.openai/hosting.json`, `hosting/product.json`, `hosting/cli.json`, `publish.mjs` and `hosting/deployments.json` describe the earlier owner-private OpenAI Sites previews. They are historical preview tooling, not Cloudflare deployment configuration. The deterministic builder currently reads the two hosting manifests for identity validation; Cloudflare serves only the selected static output and does not contact Sites or require Sites credentials. Do not run `publish.mjs` as a Cloudflare build command.

Visual QA is recorded in `design-qa.md`; ignored `qa/` holds local browser evidence. Product Hunt preparation is in `product-hunt-launch.md`; no listing or outreach has been submitted. Domain activation and actual Cloudflare deployment status must be verified separately before the story is closed.
