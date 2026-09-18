import {Client} from '../../clients/typescript/src/index.js';
export async function* snapshots(client,limit=100){
  let cursor=(await client.wait(undefined,0)).cursor;
  yield await client.top('recent',limit,0,['/role','/content','/created_at']);
  while(true){const change=await client.wait(cursor);cursor=change.cursor;if(change.changed||change.reset)yield await client.top('recent',limit,0,['/role','/content','/created_at']);}
}
if(process.argv[1]===new URL(import.meta.url).pathname){try{const client=await Client.fromConfig(process.argv[2]);for await(const history of snapshots(client))console.log(JSON.stringify(history));}catch{console.error('History sync stopped; check connection and private configuration.');process.exitCode=1;}}
