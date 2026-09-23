export type Json = null | boolean | number | string | Json[] | {[key:string]: Json};
export interface Change {cursor:string;seq:number;changed:boolean;reset:boolean}
export class BogError extends Error {status:number;code:string;retryAfterMs:number|null}
export function origin(value:string):string;
export function readPrivate(path:string):Promise<Record<string,unknown>>;
export function writePrivate(path:string,value:Record<string,unknown>,format?:'json'|'dotenv',replace?:boolean):Promise<void>;
export function createClaimable(base:string,name:string,output:string,definition?:Json):Promise<{status:'created';bog_id:string;expires_at:number;configuration:string}>;
export function authorize(base:string,path:string,onApproval?:(url:string)=>void,replace?:boolean):Promise<void>;
export function connect(base:string,path:string,onApproval?:(url:string)=>void):Promise<{status:string}>;
export function installPrivate(base:string,authFile:string,handoff:string,output:string,format?:'json'|'dotenv'):Promise<{status:string;configuration:string;bog_id:string}>;
export class Client {
 constructor(base:string,token:string,bogId?:string,workspaceId?:string);
 base:string;bogId?:string;workspaceId?:string;
 static fromConfig(path:string):Promise<Client>;
 static fromAuth(path:string,base?:string,workspaceId?:string):Promise<Client>;
 request(method:string,path:string,body?:Json,idempotencyKey?:string):Promise<any>;
 path(suffix?:string):string;
 top(resource:string,limit?:number,offset?:number,includeFields?:string[]):Promise<any>;
 previewCleanup(namePrefix:string):Promise<any>;executeCleanup(previewId:string,confirm:string):Promise<any>;deleteSandbox(confirm:string):Promise<any>;explain(id:string):Promise<any>;
 resources():Promise<any>;routes():Promise<any>;diagnostics():Promise<any>;requestStatus(id:string):Promise<any>;
 claimLink():Promise<{claim_url:string;expires_at:number;bog_id:string}>;
 get(key:string):Promise<any>;put(key:string,value:Json):Promise<any>;remove(key:string):Promise<any>;batch(ops:Json[]):Promise<any>;
 query(resource?:string,query?:Record<string,Json>):Promise<any>;
 list(resource?:string,options?:{limit?:number;after?:string;before?:string}):Promise<any>;
 batchGet(keys:string[],resource?:string):Promise<{data:{key:string;value:Json}[]}>;
 search(resource:string,query:string,limit?:number,includeFields?:string[]):Promise<any>;
 wait(cursor?:string,timeout?:number):Promise<Change>;
 create(name:string,idempotencyKey:string,definition?:Json,timeout?:number,options?:{wait?:boolean;sandbox?:boolean;appAccess?:{scope:'read'|'write';label:string}}):Promise<any>;
 addSearch(name:string,fields:string[],kind?:'semantic'|'bm25',stages?:Json[],timeout?:number):Promise<any>;
}
