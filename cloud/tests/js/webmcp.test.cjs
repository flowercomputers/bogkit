const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const code = fs.readFileSync('cloud/static/webmcp.js','utf8');
async function run(pathname, signedIn=true, legacy=false) {
  const tools = new Map(), calls = [];
  const context = { registerTool: async t => tools.set(t.name,t) };
  const document = { modelContext: legacy ? undefined : context, dispatchEvent: () => {} };
  await vm.runInNewContext(code, { document, navigator: legacy ? {modelContext:context} : {}, location:{pathname}, Event:class{},
    fetch: async (path,options) => { calls.push({path,options}); const ok = path !== '/console-session' || signedIn; return {ok, json:async()=> path==='/console-session' ? (ok?{csrf_token:'private-csrf'}:{error:{message:'sign in'}}) : {path}}; }
  });
  return {tools,calls};
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
  const {tools,calls}=await run('/console');assert.equal(tools.size,6);
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
