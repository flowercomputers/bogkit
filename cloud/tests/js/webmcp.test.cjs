const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const code = fs.readFileSync('cloud/static/webmcp.js','utf8');
async function run(pathname, signedIn=true, legacy=false) {
  const tools = new Map(), calls = [], events = [];
  const state = { signedIn, csrf: 'private-csrf', responses: new Map() };
  const context = { registerTool: async t => tools.set(t.name,t) };
  const document = { modelContext: legacy ? undefined : context, dispatchEvent: event => events.push(event) };
  await vm.runInNewContext(code, { document, navigator: legacy ? {modelContext:context} : {}, location:{pathname}, Event:class{constructor(type){this.type=type;}}, CustomEvent:class{constructor(type,{detail}){this.type=type;this.detail=detail;}},
    fetch: async (path,options) => { calls.push({path,options}); if (state.beforeFetch) await state.beforeFetch(path,options); const ok = path !== '/console-session' || state.signedIn; return {ok, json:async()=> path==='/console-session' ? (ok?{csrf_token:state.csrf}:{error:{message:'sign in'}}) : (state.responses.get(path) || {path})}; }
  });
  return {tools,calls,events,state};
}
test('public tools work in current and older WebMCP browsers without exposing auth',async()=>{
  for(const legacy of [false,true]) {
    const {tools,calls}=await run('/',false,legacy);
    assert.deepEqual([...tools.keys()],['bog_service_info','bog_templates']);
    await tools.get('bog_templates').execute({});assert.equal(calls.at(-1).path,'/v1/templates');
  }
});
test('signed-out console does not expose authenticated tools',async()=>{
  const {tools}=await run('/console',false);assert.equal(tools.size,2);
});
test('creation uses explicit workspace, stable retry key and private CSRF; default is personal',async()=>{
  const {tools,calls}=await run('/console');assert.equal(tools.size,10);
  const create=tools.get('bog_create_bog');assert.equal(create.annotations.readOnlyHint,false);
  const value=await create.execute({name:'fixture',idempotency_key:'stable'});
  assert.equal(calls.at(-1).path,'/v1/bogs');assert.equal(calls.at(-1).options.headers['Idempotency-Key'],'stable');
  assert.equal(calls.at(-1).options.headers['x-csrf-token'],'private-csrf');
  assert.equal(JSON.stringify(value).includes('private-csrf'),false);
  const workspace_id='00000000-0000-0000-0000-000000000001';
  await create.execute({name:'fixture',idempotency_key:'stable',workspace_id});
  assert.equal(calls.at(-1).path,`/v1/bogs?workspace_id=${workspace_id}`);
  assert.equal([...tools.keys()].some(n=>/token|credential|delete/.test(n)),false);
});
test('ordinary browsers retain the normal page without requiring a polyfill',async()=>{
  await vm.runInNewContext(code,{document:{},navigator:{}});
});
const bog = '12345678-1234-4123-8123-123456789abc';
const shared = '00000000-0000-0000-0000-000000000001';
test('schema and bounded previews explicitly select shared workspace, never visible selection',async()=>{
  const {tools,calls}=await run('/console');
  await tools.get('bog_schema').execute({bog_id:bog});
  assert.equal(calls.at(-1).path,`/v1/bogs/${bog}/schema`);
  const preview=tools.get('bog_preview_records');
  await preview.execute({bog_id:bog});
  assert.equal(calls.at(-1).path,`/v1/bogs/${bog}/views/docs?limit=5&offset=0`);
  await preview.execute({bog_id:bog,workspace_id:shared,limit:20,offset:10000});
  assert.equal(calls.at(-1).path,`/v1/bogs/${bog}/views/docs?limit=20&offset=10000&workspace_id=${shared}`);
  for(const args of [{limit:21},{limit:0},{limit:1.5},{limit:'5'},{limit:null},{offset:-1},{offset:10001},{workspace_id:'bad'},{bog_id:'bad'}]) {
    const before=calls.filter(c=>c.path.includes('/views/')).length;
    await assert.rejects(preview.execute({bog_id:bog,...args}));
    assert.equal(calls.filter(c=>c.path.includes('/views/')).length,before);
  }
});
test('allowances resolve personal or explicit workspace from current server data',async()=>{
  const {tools,state,calls}=await run('/console');
  state.responses.set('/v1/me',{workspace_id:bog});
  // A personal workspace shared by another person may appear first.
  state.responses.set('/v1/workspaces',{workspaces:[{id:shared,personal:true,bog_limit:null},{id:bog,personal:true,bog_limit:3}]});
  const inspect=async args=>JSON.parse((await tools.get('bog_allowance').execute(args)).content[0].text);
  assert.equal((await inspect({})).workspace_id,bog);
  assert.equal(calls.some(c=>c.path==='/v1/me'),true);
  assert.equal((await inspect({workspace_id:shared})).bog_limit,null);
  const meCalls=calls.filter(c=>c.path==='/v1/me').length;
  assert.equal((await inspect({workspace_id:bog.toUpperCase()})).workspace_id,bog);
  assert.equal(calls.filter(c=>c.path==='/v1/me').length,meCalls);
  await assert.rejects(inspect({workspace_id:'ffffffff-ffff-ffff-ffff-ffffffffffff'}),/not accessible/);
});
test('prepare app access returns only nonsecret metadata and refreshes UI without redemption',async()=>{
  const {tools,calls,events,state}=await run('/console');
  const path=`/v1/bogs/${bog}/app-access?workspace_id=${shared}`;
  state.responses.set(path,{handoff_id:bog,bog_id:bog,workspace_id:shared,scope:'read',label:'demo',expires_at:123,token:'never-output',csrf_token:'never-output',installation:'never-output'});
  state.csrf='renewed-private-csrf';
  const output=await tools.get('bog_prepare_app_access').execute({bog_id:bog,workspace_id:shared,scope:'read',label:'demo'});
  assert.equal(JSON.stringify(output).includes('never-output'),false);
  assert.equal(JSON.stringify(output).includes('renewed-private-csrf'),false);
  assert.equal(calls.at(-1).options.headers['x-csrf-token'],'renewed-private-csrf');
  assert.equal(JSON.parse(calls.at(-1).options.body).scope,'read');
  assert.equal(events.some(e=>e.type==='bog-resources-changed'),true);
  assert.equal(events.at(-1).detail.handoff_id,bog);
  assert.equal(calls.some(c=>c.path.includes('/redeem')),false);
  assert.equal([...tools.keys()].some(n=>/redeem|download/.test(n)),false);
});
test('every signed-in tool checks current session after logout, including reads',async()=>{
  const {tools,calls,state,events}=await run('/console');state.signedIn=false;
  for(const [name,tool] of tools) {
    if(['bog_service_info','bog_templates'].includes(name))continue;
    const before=calls.length;
    await assert.rejects(tool.execute({bog_id:bog,scope:'read',label:'demo',name:'demo',idempotency_key:'retry'}),/sign in/);
    assert.equal(calls.length,before+1);assert.equal(calls.at(-1).path,'/console-session');
    assert.equal(events.some(e=>e.type==='bog-tool-status'&&e.detail.state==='error'),true);
  }
});
test('cancelled browser actions do not start a request and report clear cancellation',async()=>{
  const {tools,calls,events}=await run('/console');const before=calls.length;
  await assert.rejects(tools.get('bog_prepare_app_access').execute({bog_id:bog,scope:'read',label:'demo'},{signal:{aborted:true}}),/Cancelled/);
  assert.equal(calls.length,before);
  assert.equal(events.some(e=>e.detail?.state==='cancelled'),true);
  assert.equal(events.at(-1).type,'bog-resources-changed');
});

test('in-flight cancellation propagates its signal and refreshes after an uncertain write',async()=>{
  const {tools,calls,events,state}=await run('/console');
  const signal={aborted:false};
  state.beforeFetch=async(path,options)=>{
    assert.equal(options.signal,signal);
    if(path.includes('/app-access')) {signal.aborted=true;const error=new Error('Cancelled');error.name='AbortError';throw error;}
  };
  await assert.rejects(tools.get('bog_prepare_app_access').execute({bog_id:bog,scope:'write',label:'demo'},{signal}),/Cancelled/);
  assert.equal(calls.at(-1).options.method,'POST');
  assert.equal(events.some(e=>e.detail?.state==='success'),false);
  assert.equal(events.some(e=>e.detail?.state==='cancelled'),true);
  assert.equal(events.at(-1).type,'bog-resources-changed');
  assert.equal(calls.some(c=>c.path.endsWith('/redeem')),false);
});
