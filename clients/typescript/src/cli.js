#!/usr/bin/env node
import {parseArgs} from 'node:util';
import {readFile} from 'node:fs/promises';
import {Client,connect,installPrivate,createClaimable} from './index.js';
const options=Object.fromEntries(['config','auth-file','origin','bog-id','workspace-id','input','resource','after','before','limit','query','cursor','timeout','request-id','name','idempotency-key','definition','kind','offset','handoff','output','format','name-prefix','preview-id','confirm','app-access','app-label'].map(k=>[k,{type:'string'}]));
options['include-fields']={type:'string',multiple:true};options.fields={type:'string',multiple:true};options.help={type:'boolean'};for(const flag of ['wait','no-wait','sandbox'])options[flag]={type:'boolean'};
try {
  const {values:a,positionals:[command,...args]}=parseArgs({options,allowPositionals:true});
  if(a.help){console.log('bog-cloud [--config PRIVATE_FILE | --auth-file PRIVATE_FILE] [--bog-id ID] COMMAND\nCommands: try --name NAME --output PRIVATE_FILE, claim-link, connect, resources, routes, diagnostics, get KEY, put KEY --input JSON, remove KEY, batch --input JSON, batch-get KEY..., list, search, wait, request-status ID, create, add-search, install, query, top, explain, cleanup-preview, cleanup-execute, delete-sandbox');}
  else {
    if(a.wait&&a['no-wait'])throw Error();const base=a.origin??'https://cloud.bog.new';let result;
    if(command==='install'){result=await installPrivate(base,a['auth-file'],a.handoff,a.output,a.format??'json');}
    else if(command==='connect'){if(!a['auth-file'])throw Error();result=await connect(base,a['auth-file'],url=>process.stderr.write('Approve at: '+url+'\n'));}
    else if(command==='try'){if(!a.name||!a.output)throw Error();result=await createClaimable(base,a.name,a.output,a.definition?JSON.parse(await readFile(a.definition,'utf8')):undefined);}
    else {
      const c=a.config?await Client.fromConfig(a.config):await Client.fromAuth(a['auth-file'],base,a['workspace-id']);if(a['bog-id'])c.bogId=a['bog-id'];
      const input=async path=>{if(path!=='-')return JSON.parse(await readFile(path,'utf8'));const chunks=[];for await(const part of process.stdin)chunks.push(part);return JSON.parse(Buffer.concat(chunks).toString());};const resource=a.resource??'docs';
      switch(command){
        case 'resources':case 'routes':case 'diagnostics':result=await c[command]();break;
        case 'get':case 'remove':if(!args[0])throw Error();result=await c[command](args[0]);break;
        case 'put':if(!args[0])throw Error();result=await c.put(args[0],await input(a.input));break;
        case 'batch':result=await c.batch(await input(a.input));break;
        case 'batch-get':result=await c.batchGet(args,resource);break;
        case 'list':result=await c.list(resource,{limit:Number(a.limit??100),after:a.after,before:a.before});break;
        case 'search':if(!a.query)throw Error();result=await c.search(resource,a.query,Number(a.limit??3),a['include-fields']);break;
        case 'wait':result=await c.wait(a.cursor,Number(a.timeout??25));break;
        case 'explain':case 'request-status':if(!args[0])throw Error();result=await c.requestStatus(args[0]);break;
        case 'create':if(!a.name)throw Error();result=await c.create(a.name,a['idempotency-key'],a.definition?await input(a.definition):undefined,60,{wait:!a['no-wait'],sandbox:!!a.sandbox,appAccess:a['app-access']?{scope:a['app-access'],label:a['app-label']??a.name}:undefined});break;
        case 'claim-link':result=await c.claimLink();break;
        case 'top':result=await c.top(resource,Number(a.limit??100),Number(a.offset??0),a['include-fields']);break;
        case 'query':result=await c.query(resource,await input(a.input));break;
        case 'cleanup-preview':if(!a['name-prefix'])throw Error();result=await c.previewCleanup(a['name-prefix']);break;
        case 'cleanup-execute':result=await c.executeCleanup(a['preview-id'],a.confirm);break;
        case 'delete-sandbox':result=await c.deleteSandbox(a.confirm);break;
        case 'add-search':result=await c.addSearch(a.name,a.fields,a.kind??'semantic');break;
        default:throw Error();
      }
    }
    console.log(JSON.stringify(result));
  }
}catch{console.error(JSON.stringify({error:'Operation failed. Check configuration, permission and request state before retrying writes.'}));process.exitCode=1;}
