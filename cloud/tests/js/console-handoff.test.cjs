const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const code = fs.readFileSync('cloud/static/console.js', 'utf8');
const id = '12345678-1234-4123-8123-123456789abc';
const key = 'bog.pending-app-handoff';
async function page(storage, search = '', signedIn = true, handoffStatus = 200) {
  const calls = [], nodes = new Map(), listeners = new Map();
  const node = () => ({hidden:true, value:'workspace', dataset:{}, selectedOptions:[{dataset:{role:'owner'}}], before(){}, addEventListener(){}, setAttribute(){}, querySelector(){return {focus(){}};}, focus(){}, append(){}, replaceChildren(){}, toggleAttribute(){}, scrollIntoView(){}, remove(){}, click(){return this.onclick?.();}});
  const get = name => {if (!nodes.has(name)) nodes.set(name,node()); return nodes.get(name);};
  const location = {search,pathname:'/console',hash:'',origin:'https://example.test'};
  const context = vm.createContext({URLSearchParams, location, sessionStorage:{getItem:k=>storage.get(k),setItem:(k,v)=>storage.set(k,v),removeItem:k=>storage.delete(k)},
    history:{replaceState:(_,__,path)=>{location.search=path.includes('?')?'?'+path.split('?')[1]:'';}},
    document:{querySelectorAll:()=>[],getElementById:get,createElement:node,body:node(),addEventListener(name,fn){listeners.set(name,fn);}},
    URL:{createObjectURL:()=> 'blob:private-download',revokeObjectURL(){}},Blob, setTimeout:fn=>fn(),
    fetch:async(path,options={})=>{
      calls.push({path,options});
      const status = path==='/console-session'&&!signedIn?401:path.startsWith('/v1/app-access/')?handoffStatus:200;
      let body = {};
      if(path==='/console-session') body={csrf_token:'private-csrf',account:{id:'account'}};
      if(path==='/v1/workspaces') body={workspaces:[{id:'workspace',name:'Personal',role:'owner'}]};
      if(path.startsWith('/v1/app-access/')) body=path.endsWith('/redeem')?{token:'never-store-secret',id:'token-id',bog_id:'bog'}:{handoff_id:id,label:'Fixture',scope:'read',bog_id:'bog',workspace_id:'workspace',expires_at:2000000000};
      if(status!==200) body={error:{message:'unavailable'}};
      return {status,ok:status===200,json:async()=>body};
    }});
  vm.runInContext(code,context);
  // Let the real startup chain complete, including workspace and metadata loads.
  for(let i=0;i<5;i++) await new Promise(setImmediate);
  return {calls,get,location,listeners};
}
test('signed-out handoff survives same-tab login callback and waits for explicit download',async()=>{
  const storage=new Map();
  const before=await page(storage,'?handoff='+id,false);
  assert.equal(storage.get(key),id);
  assert.equal(before.calls.some(c=>c.path.includes('/app-access/')),false);
  const after=await page(storage,'',true);
  assert.equal(after.calls.some(c=>c.path==='/v1/app-access/'+id),true);
  assert.equal(after.calls.some(c=>c.path.endsWith('/redeem')),false);
  assert.equal(after.get('app-handoff').hidden,false);
  await after.get('download-app').onclick();
  assert.equal(after.calls.filter(c=>c.path.endsWith('/redeem')).length,1);
  assert.equal(storage.has(key),false);
  assert.equal(JSON.stringify([...storage]).includes('never-store-secret'),false);
});
test('invalid UUIDs are discarded without requesting handoff metadata',async()=>{
  for(const search of ['?handoff=not-a-uuid','']) {
    const storage=new Map([[key,'not-a-uuid']]);
    const result=await page(storage,search);
    assert.equal(storage.has(key),false);
    assert.equal(result.calls.some(c=>c.path.includes('/app-access/')),false);
  }
});
test('expired, consumed, or unavailable handoffs clear the stored reference and URL',async()=>{
  for(const status of [404,403,503]) {
    const storage=new Map();const result=await page(storage,'?handoff='+id,true,status);
    assert.equal(storage.has(key),false);
    assert.equal(result.location.search,'');
    assert.equal(result.calls.some(c=>c.path.endsWith('/redeem')),false);
  }
});

test('browser preparation shows review panel and refreshes data without downloading',async()=>{
  const result=await page(new Map());
  const before=result.calls.filter(c=>c.path==='/v1/workspaces').length;
  result.listeners.get('bog-resources-changed')();
  await result.listeners.get('bog-tool-status')({detail:{state:'success',message:'Private access prepared.',handoff_id:id}});
  for(let i=0;i<5;i++) await new Promise(setImmediate);
  assert.equal(result.calls.filter(c=>c.path==='/v1/workspaces').length,before+1);
  assert.equal(result.get('app-handoff').hidden,false);
  assert.equal(result.get('status').textContent,'Private access prepared.');
  assert.equal(result.calls.some(c=>c.path.endsWith('/redeem')),false);
  await result.listeners.get('bog-tool-status')({detail:{state:'cancelled',message:'Browser action cancelled.'}});
  assert.equal(result.get('status').textContent,'Browser action cancelled.');
  await result.listeners.get('bog-tool-status')({detail:{state:'error',message:'Please sign in.'}});
  assert.equal(result.get('status').textContent,'Please sign in.');
});
