import { test } from "node:test";
import assert from "node:assert/strict";
import { createPageClient } from "../../extension/page-client.js";
import { fakePageHost,pageRow } from "./page-fixtures.js";
import { settle } from "./fake-dom.js";
const client=(host,options={})=>createPageClient({connect:host.connect,runtime:{},timeoutMs:50,...options});

test("Page client tags requests, binds file+selection and never uses one-shot Native Messaging",async()=>{
  const host=fakePageHost([pageRow(1)]);const c=client(host);const result=await c.list("");assert.equal(result.items.length,1);assert.equal(host.ports.length,1);
  assert.equal(host.requests[0].type,"page_state");assert.ok(host.requests.every((r)=>typeof r.requestId==="string"));assert.equal(host.requests[1].generation,host.generation);c.close();
});
test("lost COMMIT response on a live port can be proven only by status, with no delete resend",async()=>{
  const host=fakePageHost([pageRow(1)]);const connect=host.connect;let dropping=true;
  host.connect=()=>{const port=connect();const emit=port.onMessage.emit;port.onMessage.emit=(response)=>{if(response.type==="page_delete_result"&&dropping){dropping=false;return;}emit(response);};return port;};
  const c=client(host,{timeoutMs:10});const preview=await c.preview({scope:"filter",query:""});const result=await c.delete(preview.reviewId);
  assert.equal(result.confirmed,true);assert.equal(host.requests.filter((r)=>r.type==="page_delete").length,1);assert.equal(host.requests.at(-1).type,"page_delete_status");c.close();
});
test("port loss after deletion is unknown, never reconnect/replay/claim success",async()=>{
  const host=fakePageHost([pageRow(1)]);const connect=host.connect;
  host.connect=()=>{const port=connect();const emit=port.onMessage.emit;port.onMessage.emit=(response)=>{if(response.type==="page_delete_result"){port.disconnect();return;}emit(response);};return port;};
  const c=client(host);const preview=await c.preview({scope:"filter",query:""});await assert.rejects(c.delete(preview.reviewId),{code:"page_delete_unknown"});assert.equal(host.ports.length,1);assert.equal(host.requests.filter((r)=>r.type==="page_delete").length,1);assert.equal(host.rows.length,0);
});
test("late response after timeout cannot satisfy another request",async()=>{
  const host=fakePageHost([pageRow(1)]);let release;host.delays.page_list=()=>new Promise((resolve)=>{release=resolve;});
  const c=client(host,{timeoutMs:10});await assert.rejects(c.list(""),{code:"timeout"});delete host.delays.page_list;const fresh=await c.list("");release();await settle(5);assert.equal(fresh.items.length,1);c.close();
});
test("A→B→A generation and lock failures invalidate the client binding",async()=>{
  const host=fakePageHost([pageRow(1)]);const invalidations=[];const c=client(host,{onInvalidated:(code)=>invalidations.push(code)});
  await c.list("");host.generation="A-after-B";await c.state();assert.deepEqual(invalidations,["vault_switched"]);
  host.failure="vault_locked";await assert.rejects(c.list(""),{code:"vault_locked"});assert.equal(c.binding,null);assert.equal(invalidations.at(-1),"vault_locked");
});
test("raw errors and wrong response types never become success or leak source text",async()=>{
  const host=fakePageHost([pageRow(1)]);host.failure="https://synthetic.test/SECRET";const c=client(host);await assert.rejects(c.list(""),{code:"invalid_response"});c.close();
  const bad=fakePageHost();const connect=bad.connect;bad.connect=()=>{const port=connect();const emit=port.onMessage.emit;port.onMessage.emit=(r)=>emit({...r,type:"other"});return port;};
  const other=client(bad);await assert.rejects(other.state(),{code:"invalid_response"});other.close();
});
const exportsOf=(host)=>host.requests.filter((r)=>r.type==="page_export");
test("export names one save event, waits for the OS dialog without a timer and is sent once",async()=>{
  const host=fakePageHost([pageRow(1),pageRow(2)]);host.dialog=async()=>{await settle(40);return{fileName:"chosen.mhtml"};};
  const c=client(host,{timeoutMs:10});const row=host.rows[1];const result=await c.export(row);
  assert.deepEqual(result,{cancelled:false,fileName:"chosen.mhtml",previousFileLeft:false});
  const sent=exportsOf(host);assert.equal(sent.length,1);assert.deepEqual([sent[0].id,sent[0].saveId,sent[0].sha256],[row.id,row.saveId,row.sha256]);
  assert.deepEqual(Object.keys(sent[0]).sort(),["generation","id","requestId","saveId","sha256","type","vaultToken"]);
  host.dialog=async()=>({cancelled:true});assert.deepEqual(await c.export(row),{cancelled:true});
  host.dialog=async()=>({fileName:"again.mhtml",previousFileLeft:true});assert.equal((await c.export(row)).previousFileLeft,true);
  assert.equal(host.ports.length,1);assert.equal(exportsOf(host).length,3);c.close();
});
test("export answer lost with the port is unknown: no reconnect, no resend, no success",async()=>{
  const host=fakePageHost([pageRow(1)]);let port;const connect=host.connect;host.connect=()=>(port=connect());
  host.dialog=async()=>{port.disconnect();return{fileName:"written-or-not.mhtml"};};
  const c=client(host);await assert.rejects(c.export(host.rows[0]),{code:"page_export_unknown"});await settle(10);
  assert.equal(host.ports.length,1);assert.equal(exportsOf(host).length,1);assert.equal(c.binding,null);
});
test("export answers for another event, another original or without a file name are not success",async()=>{
  for(const override of [{id:99},{sha256:"f".repeat(64)},{fileName:null},{exported:false}]){
    const host=fakePageHost([pageRow(1)]);host.dialog=async()=>({fileName:"x.mhtml",override});
    const c=client(host);await assert.rejects(c.export(host.rows[0]),{code:"page_export_unknown"});assert.equal(exportsOf(host).length,1);c.close();
  }
});
test("export failures keep their fixed codes; a Vault change during the dialog invalidates the client",async()=>{
  const host=fakePageHost([pageRow(1)]);const invalidations=[];const c=client(host,{onInvalidated:(code)=>invalidations.push(code)});
  for(const error of ["export_file_appeared","export_target_busy","page_blob_corrupt","export_recovery_needed"]){host.dialog=async()=>({error});await assert.rejects(c.export(host.rows[0]),{code:error});}
  await assert.rejects(c.export({...host.rows[0],sha256:"e".repeat(64)}),{code:"page_export_changed"});
  await assert.rejects(c.export({...host.rows[0],id:42}),{code:"page_not_found"});
  assert.deepEqual(invalidations,[]);assert.equal(host.exports.length,0);
  // A→B→A while the dialog is open: same file token, new selection generation.
  host.dialog=async()=>{host.generation="generation-A-after-B";return{fileName:"x.mhtml"};};
  await assert.rejects(c.export(host.rows[0]),{code:"vault_switched"});assert.deepEqual(invalidations,["vault_switched"]);assert.equal(c.binding,null);assert.equal(host.exports.length,0);
});
