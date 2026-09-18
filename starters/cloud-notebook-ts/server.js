// Server-side credentials only. Exposes one page and three bounded API routes.
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {parseArgs} from 'node:util';
import {Client} from '../../clients/typescript/src/index.js';
export function notebook(client,port,page) {
  const expected=`127.0.0.1:${port}`;
  return createServer(async(req,res)=>{
    const reply=(status,value,html=false)=>{res.writeHead(status,{'Content-Type':html?'text/html; charset=utf-8':'application/json','Cache-Control':'no-store','X-Content-Type-Options':'nosniff','Content-Security-Policy':"default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'"});res.end(html?value:JSON.stringify(value));};
    if(req.headers.host!==expected||(req.headers.origin??`http://${expected}`)!==`http://${expected}`||req.headers['sec-fetch-site']==='cross-site')return reply(403,{error:'Cross-origin requests are not allowed.'});
    try {
      const url=new URL(req.url,`http://${expected}`);
      if(req.method==='GET'&&url.pathname==='/')return reply(200,page,true);
      if(req.method==='GET'&&url.pathname==='/api/notes')return reply(200,await client.list());
      if(req.method==='GET'&&url.pathname==='/api/search'){
        const query=url.searchParams.get('q'),mode=url.searchParams.get('mode')??'semantic';
        if(!query||Buffer.byteLength(query)>4096||!['semantic','words'].includes(mode))return reply(400,{error:'Invalid search.'});
        return reply(200,await client.search(mode==='semantic'?'semantic_search':'text_search',query,3,['/title','/body']));
      }
      if(req.method==='POST'&&url.pathname==='/api/notes'){
        if(req.headers['content-type']!=='application/json')return reply(415,{error:'JSON required.'});
        const chunks=[];let length=0;for await(const part of req){length+=part.length;if(length>16384)return reply(413,{error:'Note too large.'});chunks.push(part);}
        const value=JSON.parse(Buffer.concat(chunks));
        if(!value||Object.keys(value).sort().join(',')!=='body,key,title'||Object.values(value).some(v=>typeof v!=='string')||!value.key||value.key.length>128||Buffer.byteLength(value.title+value.body)>8000)return reply(400,{error:'Invalid note.'});
        return reply(200,await client.put(value.key,{title:value.title,body:value.body}));
      }
      reply(404,{error:'Not found.'});
    } catch {reply(400,{error:'Request failed. Check input, connection, or renew the private credential.'});}
  });
}
if(process.argv[1]===new URL(import.meta.url).pathname){
  try{const {values:a}=parseArgs({options:{config:{type:'string'},port:{type:'string',default:'4318'}}});const port=Number(a.port);if(!Number.isInteger(port)||port<1024||port>65535)throw Error();const client=await Client.fromConfig(a.config);await client.resources();const page=await readFile(new URL('./index.html',import.meta.url));notebook(client,port,page).listen(port,'127.0.0.1',()=>console.log(`Notebook ready at http://127.0.0.1:${port}`));}catch{console.error('Notebook failed to start. Check private configuration and selected port.');process.exitCode=1;}
}
