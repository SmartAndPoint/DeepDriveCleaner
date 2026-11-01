#!/usr/bin/env node
import {readFile,writeFile,mkdir,cp,rm} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {dirname,join,resolve} from 'node:path';
import {product,riskContent} from './src/content.mjs';
import {copy,pageMetadata} from './src/copy.mjs';
import {renderPage} from './src/pages.mjs';
const root=dirname(fileURLToPath(import.meta.url));
const args=process.argv.slice(2);
const option=(name,fallback)=>{const i=args.indexOf('--'+name);return i<0?fallback:args[i+1];};
const mode=option('navigation','companion');
if(!['companion','preview','hosted','domains'].includes(mode)) throw new Error('Unknown navigation mode');
const origins={product:option('product-origin',product.origins.product),cli:option('cli-origin',product.origins.cli)};
for(const origin of Object.values(origins)) {const u=new URL(origin);if(!['http:','https:'].includes(u.protocol)||u.username||u.password)throw new Error('Invalid navigation origin');}
const mainManifest=JSON.parse(await readFile(join(root,'hosting/product.json'),'utf8'));
const cliManifest=JSON.parse(await readFile(join(root,'hosting/cli.json'),'utf8'));
if(mainManifest.project_id===cliManifest.project_id)throw new Error('Each output must have a different Sites identity');
const development=await readFile(join(root,'src/development.html'),'utf8');
const outputRoot=resolve(option('out',join(root,'dist')));
for(const [host,manifest] of [['product',mainManifest],['cli',cliManifest]]) {
  const out=join(outputRoot,host);
  await rm(out,{recursive:true,force:true});await mkdir(join(out,'zh'),{recursive:true});
  await cp(join(root,'src/assets'),join(out,'assets'),{recursive:true});
  await cp(join(root,'src/styles.css'),join(out,'styles.css'));await cp(join(root,'src/site.js'),join(out,'site.js'));
  await writeFile(join(out,'content.js'),'window.DDC_CONTENT = '+JSON.stringify({product,riskContent,copy,pageMetadata}).replaceAll('<','\\u003c')+';\n');
  const filenames=host==='product'?{product:'index.html',cli:'cli.html',development:'development.html'}:{product:'app.html',cli:'index.html',development:'development.html'};
  const pagePaths={},destinations={},canonicals={};
  for(const lang of ['en','zh']) {
    const prefix=lang==='zh'?'zh/':'';
    pagePaths[lang]=Object.fromEntries(Object.entries(filenames).map(([p,f])=>[p,prefix+f]));
    canonicals[lang]={product:product.origins.product+'/'+prefix,cli:product.origins.cli+'/'+prefix,development:product.origins.product+'/'+prefix+'development.html'};
    destinations[lang]=Object.fromEntries(['product','cli','development'].map(page=>{
      if(mode==='companion')return[page,'/'+pagePaths[lang][page]];
      const target=page==='development'?host:page;
      const targetFilename=page==='development'?'development.html':'index.html';
      const base=mode==='domains'?product.origins[target]:origins[target];
      return[page,new URL('/'+prefix+targetFilename,base).href];
    }));
  }
  const config={destinations,canonicals,pagePaths,navigationMode:mode};
  for(const lang of ['en','zh'])for(const page of ['product','cli','development']) {
    await writeFile(join(out,pagePaths[lang][page]),renderPage(page,lang,host,config,development));
  }
  const common=[`# ${product.name} / ddc`,``,`One product: a native macOS app and the ddc CLI, sharing one engine.`,`Product: ${product.origins.product}/`,`CLI and agent guide: ${product.origins.cli}/`,`Current documented release: ${product.version}; checked ${product.checked}.`,`App: macOS 26 on Apple Silicon.`,`CLI: macOS Apple Silicon and Linux (x86_64, ARM64; static musl) archives published. A Windows build is planned.`,`Release: ${product.release}`,`CLI download (macOS): ${product.cli.macos.url}`,...product.cli.linux.archives.map(a=>`CLI download (Linux): ${a.url}`),`CLI alone on macOS or Linux: ${product.commands.script}`,`License: PolyForm Noncommercial 1.0.0 (source-available, not OSI open source).`,product.license,``,`## Install (when the user requests installation)`,product.commands.install,`Installs both the app and CLI. Open the app and grant Full Disk Access when prompted.`,`The terminal or agent host may need its own permission; app permission does not transfer.`,``,`## One workflow for a person or an agent`,`Inspect (read-only): ${product.commands.json}`,`Preview (does not modify files): ${product.commands.preview}`,`Interactive cleanup: ${product.commands.clean}; requires a terminal.`,`Default agent task: inspect and preview, then stop and ask for approval of a specific selection.`,`Do not substitute --yes for an unavailable terminal. Do not install or change permissions without authorization.`,`A scan report is not deletion consent. Cleaning deletes files.`,`--items consumes an exact caller-supplied selection without prompting or authenticating human approval.`,`clean --json requires --items. --yes also skips prompts within its scope.`,`Rechecks and protected paths reduce cleanup risks, not prompt-injection or exfiltration risks.`,`Scheduled scans do not clean. Trash is not universal; a journal is not an undo facility.`,``,...Object.entries(riskContent.en).map(([id,r])=>`${id}: ${r.description} ${r.default}`),``,`## Schemas`,...['scan','clean','rules','progress'].map(n=>`${product.code}/blob/main/docs/${n}.schema.json`),``,`## Contribute with your agent`,`The app and CLI are developed agent-first using ProjectStore.`,`${product.code}#contributing`,product.vault,product.artifact,`Prerequisites: Node.js 20+. Run setup in a normal terminal outside an agent session.`,product.commands.setup,`Then start Claude Code or Codex in the code directory.`,copy.en.agentPrompt,`Read the original contribution terms: contributions grant SmartAndPoint commercial relicensing rights.`,product.disclaimer,``,`This website does not scan or change your files.`].join('\n');
  await writeFile(join(out,'llms.txt'),common+'\n');
  await writeFile(join(out,'llms.zh.txt'),`# DeepDriveCleaner / ddc\n\n${copy.zh.sameProductText}\n${copy.zh.cliIdentity}\n${copy.zh.appOnlyMac}\n${copy.zh.faqPlatformsText}\n\n${copy.zh.agentUsagePrompt}\n\n${product.commands.json}\n${product.commands.preview}\n\n${copy.zh.apiBoundary}\n${copy.zh.noSecurityGuarantee}\n\n${copy.zh.termsDescription}\n${product.code}#contributing\n${product.vault}\n${product.artifact}\n\n${copy.zh.prerequisites}\n${product.commands.setup}\n\n${copy.zh.agentPrompt}\n\n## English technical reference\n\n${common}\n`);
  // The one-line install (story "One-line install on macOS and Linux:
  // ddc.sh/install.sh and cargo", DDC-007): ddc.sh serves the script as it
  // is in the repository, as plain text, never the page.
  if(host==='cli') {
    await cp(join(root,'../packaging/install/install.sh'),join(out,'install.sh'));
    await writeFile(join(out,'_headers'),'/install.sh\n  Content-Type: text/plain; charset=utf-8\n  Cache-Control: no-cache\n');
  }
  await writeFile(join(out,'robots.txt'),`User-agent: *\nAllow: /\nSitemap: ${product.origins[host]}/sitemap.xml\n`);
  const locs=['en','zh'].flatMap(lang=>[canonicals[lang][host],...(host==='product'?[canonicals[lang].development]:[])]);
  await writeFile(join(out,'sitemap.xml'),`<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">${locs.map(url=>`<url><loc>${url}</loc></url>`).join('')}</urlset>\n`);
  console.log(`${host}: ${out} (${manifest.project_id}, ${mode} links)`);
}
