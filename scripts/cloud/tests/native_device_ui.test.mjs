import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
const script=readFileSync(new URL('../../../cloud/static/device.js',import.meta.url),'utf8');
function fixture(){
  const nodes=new Map();
  const node=id=>{if(!nodes.has(id))nodes.set(id,{value:'',hidden:false,disabled:false,textContent:'',href:''});return nodes.get(id);};
  node('review').hidden=true;node('login').hidden=true;
  const lookups=[],decisions=[];
  const response=value=>({ok:true,status:200,json:async()=>value});
  const fetch=async(path,options)=>{
    if(path==='/console-session')return response({csrf_token:'session-csrf'});
    const body=JSON.parse(options.body);
    if(Object.hasOwn(body,'approve')){decisions.push(body);return response({approved:body.approve});}
    return await new Promise(resolve=>lookups.push({body,resolve:value=>resolve(response(value))}));
  };
  vm.runInNewContext(script,{document:{getElementById:node},location:{search:''},URLSearchParams,fetch});
  const submit=code=>{node('code').value=code;return node('lookup').onsubmit({preventDefault(){}});};
  const flush=async()=>{for(let i=0;i<10;i++)await Promise.resolve();};
  return {node,lookups,decisions,submit,flush};
}
test('an older lookup response cannot show an agent while approving a newer code',async()=>{
  const f=fixture();
  const a=f.submit('AAAAAAAA');await f.flush();
  const b=f.submit('BBBBBBBB');await f.flush();
  assert.equal(f.lookups.length,2);
  f.lookups[0].resolve({name:'Agent A',access:'A access'});await a;
  assert.equal(f.node('review').hidden,true,'stale A must not become approvable while B is pending');
  f.lookups[1].resolve({name:'Agent B',access:'B access'});await b;
  assert.equal(f.node('name').textContent,'Agent B');
  await f.node('approve').onclick();
  assert.deepEqual(f.decisions,[{user_code:'BBBBBBBB',approve:true}]);
});
test('pending lookup disables changes; accepted review stays bound even if input is edited',async()=>{
  const f=fixture();const lookup=f.submit('AAAAAAAA');await f.flush();
  assert.equal(f.node('code').disabled,true);assert.equal(f.node('lookup-submit').disabled,true);
  assert.equal(f.node('approve').disabled,true);
  f.lookups[0].resolve({name:'Agent A',access:'A access'});await lookup;
  assert.equal(f.node('code').disabled,false);assert.equal(f.node('approve').disabled,false);
  f.node('code').value='BBBBBBBB';
  await f.node('deny').onclick();
  assert.deepEqual(f.decisions,[{user_code:'AAAAAAAA',approve:false}]);
});
test('editing the public code clears the previously reviewed request',async()=>{
  const f=fixture();const lookup=f.submit('AAAAAAAA');await f.flush();f.lookups[0].resolve({name:'Agent A',access:'A access'});await lookup;
  f.node('code').value='BBBBBBBB';f.node('code').oninput();
  assert.equal(f.node('review').hidden,true);await f.node('approve').onclick();assert.equal(f.decisions.length,0);
});
test('a late stale response cannot replace the newer accepted review',async()=>{
  const f=fixture();const a=f.submit('AAAAAAAA');await f.flush();const b=f.submit('BBBBBBBB');await f.flush();
  f.lookups[1].resolve({name:'Agent B',access:'B access'});await b;
  f.lookups[0].resolve({name:'Agent A',access:'A access'});await a;
  assert.equal(f.node('name').textContent,'Agent B');
  const approval=f.node('approve').onclick();
  assert.equal(f.node('code').disabled,true);
  await f.submit('AAAAAAAA');assert.equal(f.lookups.length,2,'new lookup blocked during decision');
  await approval;assert.deepEqual(f.decisions,[{user_code:'BBBBBBBB',approve:true}]);
});
