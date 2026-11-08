#!/usr/bin/env node
// Deterministic recovery-policy checks against the production browser script.
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync(`${__dirname}/../crates/homeway-cli/src/webserve/assets/app.js`, 'utf8');
const flush = () => new Promise(resolve => setImmediate(resolve));
function deferred() { let resolve, reject; const promise = new Promise((a,b) => {resolve=a; reject=b;}); return {promise,resolve,reject}; }
function harness(hash = '') {
  const timers = new Map(), elements = new Map(), requests = [], replies = [], sockets = [], terms = [];
  let serial = 0, now = 100000;
  const location = new URL('https://example.test/' + hash);
  function element(id) {
    if (!elements.has(id)) elements.set(id, {value:'', hidden:false, disabled:false, textContent:'', listeners:{},
      addEventListener(type, fn) {this.listeners[type] = fn;}, replaceChildren(){}, focus(){}});
    return elements.get(id);
  }
  class Terminal {
    constructor(options) {this.options=options; this.unicode={}; this.parser={registerOscHandler(){}}; this.cols=80; this.rows=24; terms.push(this);}
    loadAddon(){} open(){} dispose(){this.disposed=true;} focus(){} onData(fn){this.data=fn;} onBinary(){} onResize(){} write(){} clearSelection(){} getSelection(){return '';} paste(){}
  }
  class WS {
    static OPEN = 1;
    constructor(url) {this.url=url; this.readyState=0; this.bufferedAmount=0; this.sent=[]; sockets.push(this);}
    send(data){this.sent.push(data);}
    open(){this.readyState=1; this.onopen?.();}
    message(data){this.onmessage?.({data:JSON.stringify(data)});}
    close(code=1000){this.readyState=3; queueMicrotask(() => this.onclose?.({code}));}
    fail(code){this.readyState=3; this.onclose?.({code});}
  }
  const window = {listeners:{},addEventListener(type,fn){this.listeners[type]=fn;}};
  const sandbox = {document:{getElementById:element}, window, location, history:{replaceState(a,b,url){location.href=String(url);}},
    URL, URLSearchParams, AbortController, TextEncoder, TextDecoder, Uint8Array, ArrayBuffer, Math:{...Math,random:()=>0},
    Date:{now:()=>now}, Terminal, FitAddon:{FitAddon:class {fit(){}}}, Unicode11Addon:{Unicode11Addon:class {}}, WebSocket:WS,
    navigator:{clipboard:{}}, setTimeout(fn,delay){timers.set(++serial,{fn,delay});return serial;},clearTimeout(id){timers.delete(id);},
    fetch(url, options) {
      requests.push({url,options});
      const reply = replies.shift() ?? {status:200,ticket:'ONCE'};
      if (reply.bodyPromise) {
        options.signal.addEventListener('abort',()=>reply.bodyPromise.reject(new DOMException('Aborted','AbortError')));
        return Promise.resolve({ok:true,status:200,json:()=>reply.bodyPromise.promise});
      }
      if (Object.hasOwn(reply,'jsonValue')) return Promise.resolve({ok:true,status:200,json:async()=>reply.jsonValue});
      if (reply.promise) {options.signal.addEventListener('abort',()=>reply.reject(new DOMException('Aborted','AbortError')));return reply.promise;}
      if (reply instanceof Error) return Promise.reject(reply);
      return Promise.resolve({ok:reply.status>=200&&reply.status<300,status:reply.status,json:async()=>reply});
    }};
  // Math methods are non-enumerable; retain the real implementations.
  sandbox.Math = Object.create(Math); sandbox.Math.random=()=>0;
  vm.runInNewContext(source, sandbox, {filename:'app.js'});
  const submit = (token='TOKEN') => {element('token').value=token;element('login').listeners.submit({preventDefault(){}});};
  const tick = async delay => {const next=[...timers].find(([,t])=>t.delay===delay);assert(next,`missing timer ${delay}`);timers.delete(next[0]);now+=delay;next[1].fn();await flush();};
  const ready = async () => {await flush();const ws=sockets.at(-1);assert(ws);ws.open();ws.message({type:'ready'});return ws;};
  return {element,requests,replies,sockets,terms,timers,location,window,submit,tick,ready,advance(ms){now+=ms;}};
}
(async () => {
  let h=harness('#webtoken=URL_TOKEN'); let ws=await h.ready();
  assert.equal(h.requests.length,1); assert.equal(h.requests[0].url,'/api/login'); assert.equal(ws.url,'wss://example.test/ws');
  assert.equal(h.requests[0].options.redirect,'manual');assert.equal(h.element('token').value,'');
  h.element('terminal').listeners.contextmenu({preventDefault(){this.cancelled=true;}});
  h=harness();h.submit();ws=await h.ready();assert.equal(h.location.hash,'#webtoken=TOKEN');
  ws.fail(1006);await h.tick(1000);ws=await h.ready();assert.equal(h.terms.filter(t=>!t.disposed).length,1);
  // Fast ready/close cycles retain exponential backoff; a stable connection resets it.
  ws.fail(1011);await h.tick(2000);ws=await h.ready();h.advance(30000);ws.fail(1011);assert([...h.timers.values()].some(t=>t.delay===1000));
  h.element('disconnect').listeners.click();await flush();assert.equal(h.timers.size,0);assert.equal(h.location.hash,'');
  for (const status of [400,401,403,404,413,0]) {
    h=harness();h.replies.push({status});h.submit();await flush();assert.equal(h.requests.length,1);assert.equal(h.timers.size,0,`fatal HTTP ${status}`);
  }
  for (const status of [408,429,500,503]) {
    h=harness();h.replies.push({status});h.submit();await flush();await h.tick(1000);await h.ready();
  }
  for (const jsonValue of [null,[],5,'text',{ticket:''},{ticket:' '}]) {h=harness();h.replies.push({jsonValue});h.submit();await flush();assert.equal(h.timers.size,0,'invalid success shape stops');}
  h=harness();h.replies.push({status:200});h.submit();await flush();assert.equal(h.timers.size,0,'missing ticket stops');
  h=harness();const body=deferred();h.replies.push({bodyPromise:body});h.submit();await flush();await h.tick(20000);assert([...h.timers.values()].some(t=>t.delay===1000),'HTTP200 body deadline recovers');
  h=harness();const hung=deferred();h.replies.push(hung);h.submit();await h.tick(20000);assert([...h.timers.values()].some(t=>t.delay===1000),'login deadline recovers');
  h=harness();h.submit();await flush();await h.tick(20000);assert([...h.timers.values()].some(t=>t.delay===1000),'ready deadline recovers');
  h=harness();const old=deferred();h.replies.push(old);h.submit('OLD');h.element('disconnect').listeners.click();h.submit('NEW');await h.ready();old.resolve({ok:true,status:200,json:async()=>({ticket:'STALE'})});await flush();assert.equal(h.sockets.length,1);assert.equal(h.location.hash,'#webtoken=NEW');
  // Authentication grace belongs to one outage; HTTP401 failures never reset it.
  h=harness();h.submit();ws=await h.ready();ws.fail(1008);
  for (const delay of [1000,2000,4000,8000,16000,30000]) {h.replies.push({status:401});await h.tick(delay);}
  assert.equal(h.timers.size,0);assert.equal(h.requests.length,7,'one successful login plus six failed reauths');
  // A pre-ready1008 during recovery is retried, while initial invalid tickets stop.
  h=harness();h.submit();ws=await h.ready();ws.fail(1008);await h.tick(1000);h.sockets.at(-1).open();h.sockets.at(-1).fail(1008);await h.tick(2000);await h.ready();
  h=harness();h.submit();await flush();h.sockets[0].open();h.sockets[0].fail(1008);assert.equal(h.timers.size,0);
  for (const code of ['revoked','terminal_unavailable','protocol_error']) {h=harness();h.submit();ws=await h.ready();ws.message({type:'error',code});ws.fail(1008);assert.equal(h.timers.size,0,code);}
  for (const code of [1000,1002,1009]) {h=harness();h.submit();ws=await h.ready();ws.fail(code);assert.equal(h.timers.size,0,`close ${code}`);}
  h=harness();h.submit();ws=await h.ready();ws.message({type:'exit',status:0});ws.fail(1000);assert.equal(h.timers.size,0);assert.equal(h.element('reconnect').hidden,false);
  console.log('PASS: URL/manual token, HTTP/WS retry boundaries, deadlines, cancellation, stale login, bounded auth grace, backoff and TUI exit');
})().catch(error=>{console.error(error);process.exitCode=1;});
