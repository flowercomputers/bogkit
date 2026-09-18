import test from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:net';
import {notebook} from '../../../starters/cloud-notebook-ts/server.js';
import {snapshots} from '../../../starters/cloud-chat/sync.mjs';
test('history captures cursor before snapshot and refetches on reset',async()=>{const calls=[];let waits=0;const client={wait:async(cursor,timeout)=>{calls.push(['wait',cursor,timeout]);return ++waits===1?{cursor:'initial'}:{cursor:'reset',changed:false,reset:true};},top:async(...args)=>{calls.push(['top',...args]);return {data:[]};}};const stream=snapshots(client);await stream.next();await stream.next();await stream.return();assert.deepEqual(calls,[['wait',undefined,0],['top','recent',100,0,['/role','/content','/created_at']],['wait','initial',undefined],['top','recent',100,0,['/role','/content','/created_at']]]);});
test('notebook persists through client, protects origins, never serves config',async()=>{
 const probe=createServer();await new Promise(r=>probe.listen(0,'127.0.0.1',r));const port=probe.address().port;await new Promise(r=>probe.close(r));
 const records=new Map();const client={list:async()=>({records:[...records]}),put:async(k,v)=>{records.set(k,v);return {ok:true};},search:async()=>[]};const server=notebook(client,port,Buffer.from('<html>Notebook</html>'));await new Promise(r=>server.listen(port,'127.0.0.1',r));const base=`http://127.0.0.1:${port}`;
 try{
  assert.match(await(await fetch(base)).text(),/Notebook/);
  assert.equal((await fetch(base+'/api/notes',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({key:'first',title:'Saved',body:'persisted'})})).status,200);
  assert.deepEqual(await(await fetch(base+'/api/notes')).json(),{records:[['first',{title:'Saved',body:'persisted'}]]});
  assert.equal((await fetch(base+'/api/notes',{headers:{Origin:'https://evil.example'}})).status,403);
  assert.equal((await fetch(base+'/private.json')).status,404);
  assert.equal((await fetch(base+'/api/notes',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({key:'first',title:3,body:'bad'})})).status,400);
 }finally{await new Promise(r=>server.close(r));}
});
