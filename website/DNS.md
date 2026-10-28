# Cloudflare Pages domains

The production host selected by the owner on 28 October 2026 is Cloudflare Pages. The previous OpenAI Sites deployments remain private design previews. Their apex A records and verification TXT records are not the DNS instructions for production; do not copy them into the new zones.

## Connect the two primary domains

1. Add `deepdrivecleaner.app` and `ddc.sh` as zones in the owner's Cloudflare account. Review existing DNS records, including any mail records, before changing authoritative nameservers.
2. At the domain registrar, replace the authoritative nameservers with the exact pair Cloudflare assigns to each zone. Domain registration stays at the registrar; no registrar transfer is required. Never guess nameserver hostnames.
3. Wait until Cloudflare shows both zones active.
4. In Workers & Pages, open the product Pages project, choose Custom domains > Set up a domain, and enter `deepdrivecleaner.app`. In the CLI project, add `ddc.sh` in the same way. Complete this project association before relying on DNS records; a manually created CNAME alone is insufficient.
5. Let the Pages custom-domain flow create the appropriate DNS records and certificates. Check for conflicting old apex records only on these hostnames; preserve unrelated MX, TXT and service records.
6. Require an active custom-domain state and valid TLS for both HTTPS origins. Verify each from an anonymous browser without ChatGPT sign-in.
7. Check English and `/zh/` on both sites, cross-links, installation/release links and the development runbook. The production build already uses `--navigation domains`, so both domains must resolve for those cross-links to work.

The screenshot-supplied Pages configuration has been confirmed by the owner, but successful builds, exact project URLs, nameserver changes, custom-domain activation and TLS have not yet been independently verified. No fabricated record values are supplied here.

## deepdrivecleaner.com

Use a permanent HTTPS redirect to `https://deepdrivecleaner.app`, preserving path and query parameters. This can be a Cloudflare redirect rule after that zone and proxied hostname are configured, or HTTPS forwarding offered by the registrar. A DNS record alone cannot perform an HTTP redirect. Verify both HTTP and HTTPS requests and the final location before claiming the redirect works.

## Automatic publication

Source repository: `SmartAndPoint/DeepDriveCleaner`; root directory: `website`; production branch: `main`. Build both projects with `node package.mjs --navigation domains`. Publish `dist/product` and `dist/cli` respectively. Set `NODE_VERSION=22` and build watch include `website/*` in both projects.

Official Cloudflare [custom-domain instructions](https://developers.cloudflare.com/pages/configuration/custom-domains/) and [Git integration instructions](https://developers.cloudflare.com/pages/get-started/git-integration/) are the source for this procedure.

No registrar DNS mutation, Cloudflare activation, Product Hunt submission or external outreach was performed by this documentation change.
