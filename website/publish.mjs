#!/usr/bin/env node
import {spawnSync} from 'node:child_process';
import {readFile,rm,cp} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {dirname,join} from 'node:path';
const root=dirname(fileURLToPath(import.meta.url));
const [site,...options]=process.argv.slice(2);
if(!['product','cli'].includes(site))throw new Error('Choose product or cli');
const selected=JSON.parse(await readFile(join(root,'hosting',site+'.json'),'utf8'));
const manifest=JSON.parse(await readFile(join(root,'.openai/hosting.json'),'utf8'));
if(manifest.project_id!==selected.project_id||manifest.static?.directory!=='dist')throw new Error('Publication manifest does not match selected Site');
const temporary=join(root,'.site-build');
const built=spawnSync(process.execPath,[join(root,'package.mjs'),...options,'--out',temporary],{stdio:'inherit'});
if(built.status!==0)process.exit(built.status??1);
await rm(join(root,'dist'),{recursive:true,force:true});
await cp(join(temporary,site),join(root,'dist'),{recursive:true});
console.log('Publication output: '+site+' -> dist');
