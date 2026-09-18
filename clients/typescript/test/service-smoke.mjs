// Explicit actual-service canary. Creates and deletes only its own named sandbox.
// BOG_TEST_ORIGIN and BOG_TEST_AUTH_FILE must identify an authorized test service.
import {Client,installPrivate} from '../src/index.js';
import {mkdtemp,readFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {randomUUID} from 'node:crypto';
import assert from 'node:assert/strict';
const base=process.env.BOG_TEST_ORIGIN,authFile=process.env.BOG_TEST_AUTH_FILE;
if(!base||!authFile)throw new Error('Set BOG_TEST_ORIGIN and private BOG_TEST_AUTH_FILE.');
const c=await Client.fromAuth(authFile,base,process.env.BOG_TEST_WORKSPACE_ID);
const temporary=await mkdtemp(join(tmpdir(),'bog-service-smoke-'));
try{
 const definition=JSON.parse(await readFile(new URL('../../../docs/examples/composable/todo-search.json',import.meta.url),'utf8'));
 const created=await c.create('client-smoke-'+randomUUID(),randomUUID(),definition,60,{sandbox:true,wait:true,appAccess:{scope:'read',label:'Client smoke'}});
 assert.equal(created.status,'ready');assert.ok(created.app_access.handoff_id);
 await installPrivate(base,authFile,created.app_access.handoff_id,join(temporary,'app.env'),'dotenv');
 const app=await Client.fromConfig(join(temporary,'app.env'));
 const before=(await c.wait(undefined,0)).cursor;
 await c.put('b',{title:'smoke alpha',notes:'findable',priority:9,completed:false});
 await c.put('c',{title:'smoke beta',notes:'findable',priority:3,completed:false});
 assert.equal((await c.wait(before,0)).changed,true);
 const batch=await app.batchGet(['b','missing','b']);assert.equal(batch.data.length,3);assert.equal(batch.data[1].value,null);assert.deepEqual(batch.data[0],batch.data[2]);
 const page=await app.list('docs',{after:'a',before:'d',limit:10});assert.ok(JSON.stringify(page).includes('smoke alpha'));
 const rank=await app.top('priority',10,0,['/title']);assert.equal(rank.data[0].value['/title'],'smoke alpha');
 const search=await app.search('text_search','smoke',3,['/title']);assert.ok(JSON.stringify(search).includes('/title'));
 await app.routes();await app.diagnostics();
 await assert.rejects(app.put('forbidden',{title:'no'}));
 console.log(JSON.stringify({status:'passed',checks:['sandbox-readiness','private-install-dotenv','batch-get-null-duplicates','range','rank','search-projection','change-wait','routes','diagnostics','read-scope']}));
}finally{
 try{if(c.bogId)await c.deleteSandbox(c.bogId);}finally{await rm(temporary,{recursive:true,force:true});}
}
