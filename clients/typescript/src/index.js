import {open, constants, lstat, stat, rename, unlink} from 'node:fs/promises';
import {resolve, dirname} from 'node:path';
import {randomUUID} from 'node:crypto';
const codes = new Set(['capacity','rate_limited','conflict','writes_paused','unauthorized','forbidden','not_found','invalid_request','authorization_pending','slow_down','access_denied','expired_token']);
export class BogError extends Error {
  constructor(status, code='request_rejected',retryAfterMs=null) { super(`Bog request failed (${status}, ${codes.has(code)?code:'request_rejected'}).`); this.status=status; this.code=codes.has(code)?code:'request_rejected';this.retryAfterMs=retryAfterMs; }
}
export function origin(value) {
  let u; try {u=new URL(value);} catch {throw new Error('Invalid service origin.');}
  if(u.username || u.password || u.search || u.hash || u.pathname!=='/' || (u.protocol!=='https:' && !(u.protocol==='http:' && ['localhost','127.0.0.1','[::1]'].includes(u.hostname)))) throw new Error('Use HTTPS or loopback HTTP.');
  return u.origin;
}
function dotenvValue(raw) {
  if(raw.startsWith("'")){
    if(raw.length<2||!raw.endsWith("'"))throw new Error('Invalid dotenv quotation.');
    let value='';for(let i=1;i<raw.length-1;i++){let c=raw[i];if(c==='\\'){c=raw[++i];if(i>=raw.length-1||!['\\',"'"].includes(c))throw new Error('Invalid dotenv escape.');}else if(c==="'")throw new Error('Invalid dotenv quotation.');value+=c;}return value;
  }
  try{return JSON.parse(raw);}catch{return raw;}
}
export async function readPrivate(path) {
  const file=await open(path, constants.O_RDONLY|constants.O_NOFOLLOW|constants.O_NONBLOCK);
  try {
    const info=await file.stat();
    if(!info.isFile() || (info.mode&0o777)!==0o600 || info.uid!==process.getuid()) throw new Error('Configuration must be an owned regular mode-600 file.');
    const text=await file.readFile('utf8'); let value;
    try {value=JSON.parse(text);} catch {
      value={}; for(const line of text.split('\n')) { if(!line.trim()||line.trimStart().startsWith('#'))continue; const at=line.indexOf('='); if(at<1)throw new Error('Invalid configuration.'); const raw=line.slice(at+1).trim(); value[line.slice(0,at).trim()]=dotenvValue(raw); }
    }
    if(!value || typeof value!=='object' || Array.isArray(value))throw new Error('Invalid configuration.');
    return value;
  } finally {await file.close();}
}
export async function writePrivate(path,value,format='json',replace=false) {
  if(replace){try{await readPrivate(path);}catch(e){if(e.code!=='ENOENT')throw e;}}
  const target=replace?resolve(dirname(path),'.bog-'+randomUUID()):path;
  const file=await open(target,'wx',0o600);
  try {try{await file.writeFile(format==='dotenv'?Object.entries(value).map(([k,v])=>`${k}=${JSON.stringify(v)}`).join('\n')+'\n':JSON.stringify(value)+'\n');await file.sync();}finally{await file.close();}if(replace)await rename(target,path);}finally{if(replace)await unlink(target).catch(()=>{});}
}
const pause=ms=>new Promise(resolve=>setTimeout(resolve,ms));
export class Client {
  constructor(base,token,bogId,workspaceId) {this.base=origin(base); if(typeof token!=='string'||!token.trim()||/[\x00-\x1f\x7f]/.test(token))throw new Error('Invalid private credential.');this._token=token;this.bogId=bogId;this.workspaceId=workspaceId;}
  static async fromConfig(path) {const v=await readPrivate(path);for(const k of ['BOG_CLOUD_URL','BOG_CLOUD_TOKEN','BOG_ID'])if(typeof v[k]!=='string'||!v[k].trim()||/[\x00-\x1f\x7f]/.test(v[k]))throw new Error('Invalid private configuration.');return new Client(v.BOG_CLOUD_URL,v.BOG_CLOUD_TOKEN,v.BOG_ID);}
  static async fromAuth(path,base='https://cloud.bog.new',workspaceId) {const v=await readPrivate(path);if(v.origin!==origin(base)||!Number.isFinite(v.expires_at)||v.expires_at<=Date.now()/1000+30)throw new Error('Authorization origin mismatch or expired.');return new Client(base,v.access_token,undefined,workspaceId);}
  async request(method,path,body,idempotencyKey) {
    const deadline=Date.now()+30000;
    for(let attempt=0;;attempt++){
      try{return await this._requestOnce(method,path,body,idempotencyKey,Math.max(1,deadline-Date.now()));}catch(error){
        if(!(error instanceof BogError)||error.status!==429||error.retryAfterMs===null||attempt>=2||Date.now()+error.retryAfterMs>deadline)throw error;
        await pause(error.retryAfterMs);
      }
    }
  }
  async _requestOnce(method,path,body,idempotencyKey,timeoutMs=35000) {
    if(this.workspaceId)path+=(path.includes('?')?'&':'?')+new URLSearchParams({workspace_id:this.workspaceId});
    let r;try {r=await fetch(this.base+path,{method,redirect:'error',signal:AbortSignal.timeout(timeoutMs),headers:{Accept:'application/json',Authorization:`Bearer ${this._token}`,...(body===undefined?{}:{'Content-Type':'application/json'}),...(idempotencyKey?{'Idempotency-Key':idempotencyKey}:{})},body:body===undefined?undefined:JSON.stringify(body)});}catch {throw new Error('Connection failed; a write may have completed. Inspect state before retrying.');}
    const reader=r.body?.getReader();let size=0;const chunks=[];if(reader)while(true){const {done,value}=await reader.read();if(done)break;size+=value.length;if(size>5*1024*1024){await reader.cancel();throw new Error('Response too large.');}chunks.push(value);}
    let v;try{const text=Buffer.concat(chunks).toString();v=text?JSON.parse(text):null;}catch{throw new Error('Invalid service response.');}
    if(!r.ok){let delay=null;if(typeof v?.retry_after_ms==='number'&&Number.isFinite(v.retry_after_ms)&&v.retry_after_ms>=0)delay=v.retry_after_ms;else{const header=r.headers.get('Retry-After');if(header!==null&&header.trim()!==''&&Number.isFinite(Number(header))&&Number(header)>=0)delay=Number(header)*1000;}throw new BogError(r.status,typeof v?.error==='string'?v.error:v?.error?.code,delay);}return v;
  }
  path(suffix=''){if(!this.bogId)throw new Error('Select a Bog first.');return `/v1/bogs/${encodeURIComponent(this.bogId)}${suffix}`;}
  top(resource,limit=100,offset=0,includeFields){return this.query(resource,{action:'top',limit,offset,...(includeFields===undefined?{}:{include_fields:includeFields})});}
  previewCleanup(namePrefix){return this.request('POST','/v1/bogs/cleanup/preview',{name_prefix:namePrefix});}
  executeCleanup(previewId,confirm){if(typeof previewId!=='string'||!previewId||confirm!==previewId)throw new Error('Confirm the exact cleanup preview ID.');return this.request('POST','/v1/bogs/cleanup/execute',{preview_id:previewId});}
  deleteSandbox(confirm){if(!this.bogId||confirm!==this.bogId)throw new Error('Confirm the exact sandbox Bog ID.');return this.request('DELETE',this.path(),{confirm});}
  explain(id){return this.requestStatus(id);}
  resources(){return this.request('GET',this.path('/resources'));}
  routes(){return this.request('GET',this.path('/routes'));}
  get(key){return this.request('GET',this.path('/docs/'+encodeURIComponent(key)));}
  put(key,value){return this.request('PUT',this.path('/docs/'+encodeURIComponent(key)),value);}
  remove(key){return this.request('DELETE',this.path('/docs/'+encodeURIComponent(key)));}
  batch(ops){return this.request('POST',this.path('/batch'),{ops});}
  query(resource='docs',query={}){return this.request('POST',this.path('/resources/'+encodeURIComponent(resource)+'/query'),query);}
  list(resource='docs',{limit=100,after,before}={}){return this.query(resource,{limit,...(after===undefined?{}:{after}),...(before===undefined?{}:{before})});}
  batchGet(keys,resource='docs'){if(!Array.isArray(keys)||keys.length<1||keys.length>100||keys.some(k=>typeof k!=='string'||!k))throw new Error('batchGet requires 1 through 100 nonempty keys.');return this.query(resource,{action:'batch_get',keys});}
  search(resource,query,limit=3,includeFields){return this.request('POST',this.path('/resources/'+encodeURIComponent(resource)+'/search'),{query,limit,...(includeFields===undefined?{}:{include_fields:includeFields})});}
  wait(cursor,timeout=25){if(!Number.isInteger(timeout)||timeout<0||timeout>25)throw new Error('Wait timeout must be 0 through 25 seconds.');return this.request('GET',this.path('/changes?')+new URLSearchParams({timeout:String(timeout),...(cursor===undefined?{}:{cursor})}));}
  diagnostics(){return this.request('GET',this.path('/usage'));}
  requestStatus(id){return this.request('GET','/v1/requests/'+encodeURIComponent(id));}
  async create(name,idempotencyKey,definition,timeout=60,{wait=true,sandbox=false,appAccess}={}){if(typeof wait!=='boolean'||typeof sandbox!=='boolean')throw new Error('wait and sandbox must be booleans.');if(!idempotencyKey)throw new Error('Creation requires a stable idempotency key.');let r=await this.request('POST','/v1/bogs',{name,wait,sandbox,...(appAccess===undefined?{}:{app_access:appAccess}),...(definition===undefined?{}:{definition})},idempotencyKey);this.bogId=r.id;const access=r.app_access;if(!wait)return {...r,bog_id:this.bogId};const end=Date.now()+timeout*1000;while(r.status!=='ready'){if(r.status==='failed'||Date.now()>=end)throw new Error('Startup incomplete; retain the Bog ID and original idempotency key.');await pause(500);r=await this.request('GET',this.path());}return {bog_id:this.bogId,status:'ready',resources:await this.resources(),...(access===undefined?{}:{app_access:access})};}
  async addSearch(name,fields,kind='semantic',stages=[],timeout=120){if(!['semantic','bm25'].includes(kind)||! /^[a-z][a-z0-9_]{0,47}$/.test(name)||!Array.isArray(fields)||!fields.length||fields.some(f=>typeof f!=='string'||!f.startsWith('/')))throw new Error('Invalid additive search definition.');const current=await this.request('GET',this.path('/definition'));const definition=structuredClone(current.definition);if(name in definition.resources||name in definition.expose)throw new Error('Resource already exists.');definition.resources[name]={stages,terminal:{kind,fields}};definition.expose[name]={target:name,action:'search'};const body={definition,expected_revision:current.revision};await this.request('POST',this.path('/definition/plan'),body);let job=await this.request('POST',this.path('/definition/apply'),body);const end=Date.now()+timeout*1000;while(job.status!=='succeeded'){if(['failed','recovery_required'].includes(job.status)||Date.now()>=end)throw new Error('Search activation incomplete; inspect definition jobs before retrying.');await pause(500);job=await this.request('GET',this.path('/definition/jobs/'+encodeURIComponent(job.job_id)));}return job;}
}
export async function authorize(base,path,onApproval=()=>{},replace=false) {
  const c=new Client(base,'device-approval');const device=await c.request('POST','/auth/device',{name:'Bog Cloud client'});
  const approval=new URL(device.verification_uri);if(approval.origin!==c.base)throw new Error('Approval origin mismatch.');approval.searchParams.set('user_code',device.user_code);onApproval(approval.href);
  let interval=Math.max(5,Number(device.interval)||5);const end=Date.now()+Math.min(600,Number(device.expires_in)||600)*1000;
  while(Date.now()<end){await pause(interval*1000);try{const r=await c.request('POST','/auth/device/token',{device_code:device.device_code});if(typeof r.access_token!=='string'||!r.access_token)throw new Error('Invalid authorization result.');await writePrivate(path,{origin:c.base,access_token:r.access_token,expires_at:Math.floor(Date.now()/1000)+(r.expires_in??2592000)},'json',replace);return;}catch(e){if(e instanceof BogError&&e.code==='slow_down')interval+=5;else if(!(e instanceof BogError&&e.code==='authorization_pending'))throw e;}}
  throw new Error('Device approval expired.');
}

export async function installPrivate(base,authFile,handoff,output,format='json') {
  if(!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(handoff)||!['json','dotenv'].includes(format))throw new Error('Invalid installation arguments.');
  if(resolve(authFile)===resolve(output)||!(await stat(dirname(resolve(output)))).isDirectory())throw new Error('Select a new output in an existing private directory.');
  try {await lstat(output);throw new Error('Output already exists.');} catch(e){if(e.code!=='ENOENT')throw e;}
  const client=await Client.fromAuth(authFile,base);
  await client.request('GET','/v1/app-access/'+handoff);
  let result;try{result=await client.request('POST','/v1/app-access/'+handoff+'/redeem',{});}catch{throw new Error('Redemption outcome uncertain. Prepare a new handoff and revoke any unused credential.');}
  try{const config={BOG_CLOUD_URL:client.base,BOG_ID:result.bog_id,BOG_CLOUD_TOKEN:result.token,credential_id:result.id};for(const key of Object.keys(config))if(typeof config[key]!=='string'||!config[key]||/[\x00-\x1f\x7f]/.test(config[key]))throw Error();await writePrivate(output,config,format);}catch{throw new Error('Private delivery failed after redemption. Revoke the issued credential before preparing a new handoff.');}
  return {status:'installed',configuration:output,bog_id:result.bog_id};
}

export async function connect(base,path,onApproval=()=>{}) {
  base=origin(base);let saved;try{saved=await readPrivate(path);}catch(e){if(e.code!=='ENOENT')throw e;}
  if(saved&&saved.origin!==base)throw new Error('Authorization origin mismatch.');
  if(saved&&Number.isFinite(saved.expires_at)&&saved.expires_at>Date.now()/1000+30){
    const client=await Client.fromAuth(path,base);try{await client.request('GET','/v1/me');return {status:'connected'};}catch(e){if(!(e instanceof BogError)||e.status!==401)throw e;}
  }
  await authorize(base,path,onApproval,!!saved);
  const client=await Client.fromAuth(path,base);await client.request('GET','/v1/me');return {status:'connected'};
}
