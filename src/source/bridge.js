// Adapted from the user's Yuin desktop LX bridge; see docs/THIRD_PARTY_NOTICES.md.
// A small LX API 2.0 bridge. Native functions expose HTTP and byte/crypto operations only.
(() => {
  'use strict';
  globalThis.window = globalThis;
  const native = (op, args) => { const r = JSON.parse(__native(op, JSON.stringify(args))); if (r.error) throw new Error(r.error); return r.value; };
  class Bytes extends Uint8Array {
    static from(value, encoding = 'utf8') {
      if (typeof value === 'string') return new Bytes(native('encode', {value, encoding}));
      if (value instanceof ArrayBuffer) return new Bytes(value.slice(0));
      if (value && value.type === 'Buffer') value = value.data;
      return new Bytes(value || []);
    }
    static alloc(n, fill = 0) { if (!Number.isInteger(n) || n < 0 || n > 32000000) throw new Error('Invalid buffer size'); return new Bytes(n).fill(fill); }
    static concat(values) { const b = Bytes.alloc(values.reduce((n,v)=>n+v.length,0)); let i=0; for(const v of values){b.set(v,i);i+=v.length;} return b; }
    toString(encoding='utf8') { return native('decode',{value:Array.from(this),encoding}); }
    toJSON() { return {type:'Buffer',data:Array.from(this)}; }
  }
  const array = value => Array.from(Bytes.from(value));
  const timers = new Map(); let timerID=0;
  globalThis.setTimeout = (fn,delay=0,...args) => { const id=++timerID; timers.set(id,()=>{timers.delete(id);fn(...args);}); __schedule(id,Math.max(0,Number(delay)||0)); return id; };
  globalThis.clearTimeout = id => timers.delete(id);
  globalThis.setInterval = (fn,delay=0,...args) => { const id=++timerID; timers.set(id,()=>{fn(...args);if(timers.has(id))__schedule(id,Math.max(10,Number(delay)||0));}); __schedule(id,Math.max(10,Number(delay)||0)); return id; };
  globalThis.clearInterval = globalThis.clearTimeout;
  globalThis.__timer = id => { const fn=timers.get(id); if(fn) fn(); };
  globalThis.atob = str => Bytes.from(str,'base64').toString('binary');
  globalThis.btoa = str => Bytes.from(str,'binary').toString('base64');
  globalThis.TextEncoder = class { encode(s){return Bytes.from(String(s));} };
  globalThis.TextDecoder = class { decode(b){return Bytes.from(b).toString();} };
  globalThis.console = {log(){},warn(){},error(){},info(){},debug(){}};
  let requestHandler=null, initID=null, initialized=false, updateSent=false, requestID=0;
  const callbacks=new Map();
  const emit = value => __emit(JSON.stringify(value));
  globalThis.__httpReply = (id,error,response) => {
    const callback=callbacks.get(id);callbacks.delete(id);if(!callback)return;
    if(error) { callback(new Error(error),null,null); return; }
    response.raw=Bytes.from(response.raw,'base64');response.bytes=response.raw.length;
    callback(null,response,response.body);
  };
  globalThis.lx = {
    version:'2.0.0',env:'desktop',EVENT_NAMES:{request:'request',inited:'inited',updateAlert:'updateAlert'},currentScriptInfo:{},
    on(event,fn) { if(event!=='request'||typeof fn!=='function')return Promise.reject(new Error('Unsupported event'));requestHandler=fn;return Promise.resolve(); },
    send(event,data) {
      if(event==='inited') {
        if(initialized)return Promise.reject(new Error('Script is inited'));initialized=true;
        try {
          if(!data || !data.sources) throw new Error('Missing source capabilities');
          const sources={};
          for(const p of ['kw','kg','tx','wy','mg','local']) { const s=data.sources[p];if(!s||s.type!=='music')continue;
            if(!Array.isArray(s.actions)||!Array.isArray(s.qualitys))throw new Error('Invalid source capabilities');
            sources[p]={type:'music',actions:(p==='local'?['musicUrl','lyric','pic']:['musicUrl']).filter(x=>s.actions.includes(x)),qualitys:(p==='local'?[]:['128k','320k','flac','flac24bit']).filter(x=>s.qualitys.includes(x))};
          }
          if(!requestHandler)throw new Error('Request event is not defined');
          emit({id:initID,result:{sources}});return Promise.resolve();
        } catch(e){emit({id:initID,error:e.message});return Promise.reject(e);}
      }
      if(event==='updateAlert') { if(updateSent)return Promise.reject(new Error('Update already reported'));updateSent=true;emit({update:String(data?.log||'音乐源有新版本').slice(0,1024)});return Promise.resolve(); }
      return Promise.reject(new Error('Unsupported event'));
    },
    request(url,options,callback) {
      const id=++requestID;callbacks.set(id,callback);__http(id,String(url),JSON.stringify(options||{}));
      return ()=>{callbacks.delete(id);__cancelHTTP(id);};
    },
    utils:{
      buffer:{from:(...args)=>Bytes.from(...args),bufToString:(b,format)=>Bytes.from(b,typeof b==='string'?'binary':undefined).toString(format)},
      crypto:{
        md5:value=>native('md5',{value:array(value)}),
        randomBytes:size=>new Bytes(native('random',{size})),
        aesEncrypt:(buffer,mode,key,iv)=>new Bytes(native('aes',{value:array(buffer),mode,key:array(key),iv:array(iv)})),
        rsaEncrypt:(buffer,key)=>new Bytes(native('rsa',{value:array(buffer),key:typeof key==='string'?key:Bytes.from(key).toString()}))
      },
      zlib:{inflate:buf=>Promise.resolve().then(()=>new Bytes(native('inflate',{value:array(buf)}))),deflate:buf=>Promise.resolve().then(()=>new Bytes(native('deflate',{value:array(buf)})))}
    }
  };
  globalThis.__command = command => {
    if(command.op==='init') {
      initID=command.id;lx.currentScriptInfo=command.meta;
      try { (0,eval)(command.script); } catch(e) { emit({id:initID,error:String(e.message||e)}); }
    } else if(command.op==='resolve') {
      Promise.resolve().then(()=>{if(!requestHandler)throw new Error('Source not initialized');return requestHandler.call(lx,{source:command.source,action:command.action,info:command.info});})
        .then(media=>{const result=typeof media==='string'?{url:media}:media;if(!result||typeof result.url!=='string'||result.url.length>8192||!/^https?:\/\//i.test(result.url))throw new Error('Invalid music URL');emit({id:command.id,result});})
        .catch(e=>emit({id:command.id,error:String(e.message||e)}));
    }
  };
})();
