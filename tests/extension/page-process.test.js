// Fake DOM/Blob, REAL persistent Native Host + MCP, synthetic temp APP_DIR/Vault only.
// Not a real Chrome capture/modal/layout test.
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync,rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import { createPageClient } from "../../extension/page-client.js";
import { createPagePanel } from "../../extension/page-panel.js";
import { captureAndUploadPage } from "../../extension/page-upload.js";
import { createDocument,settle } from "./fake-dom.js";
import { seedLegacyPolicy,startMcp,confirmPublication } from "./real-processes.js";

const hostBin=process.env.MEMOCAMPUS_TEST_HOST,mcpBin=process.env.MEMOCAMPUS_TEST_MCP;
test("Page UI with real Host: grouped metadata, confirmed shared-blob deletion, no Page in approved MCP",{skip:hostBin&&mcpBin?false:"set MEMOCAMPUS_TEST_HOST and MEMOCAMPUS_TEST_MCP to run"},async()=>{
  const app=mkdtempSync(path.join(os.tmpdir(),"memocampus-page-s2-"));const env={...process.env,MEMOCAMPUS_APP_DIR:app};delete env.MEMOCAMPUS_DB;seedLegacyPolicy(app);
  const children=new Set();const event=()=>{const listeners=[];return{addListener:(f)=>listeners.push(f),emit:(v)=>listeners.forEach((f)=>f(v))};};
  const connect=()=>{
    const child=spawn(hostBin,[],{env,cwd:app,windowsHide:true,stdio:["pipe","pipe","pipe"]});children.add(child);let buffer=Buffer.alloc(0);
    const port={onMessage:event(),onDisconnect:event(),postMessage(message){const body=Buffer.from(JSON.stringify(message));const head=Buffer.alloc(4);head.writeUInt32LE(body.length);child.stdin.write(Buffer.concat([head,body]));},disconnect(){child.kill();}};
    child.stdin.on("error",()=>{});child.on("error",()=>port.onDisconnect.emit());child.on("close",()=>{children.delete(child);port.onDisconnect.emit();});child.stderr.resume();
    child.stdout.on("data",(chunk)=>{buffer=Buffer.concat([buffer,chunk]);while(buffer.length>=4){const len=buffer.readUInt32LE();assert.ok(len<1024*1024);if(buffer.length<len+4)break;const value=JSON.parse(buffer.subarray(4,4+len));buffer=buffer.subarray(4+len);port.onMessage.emit(value);}});return port;
  };
  const call=(message)=>new Promise((resolve,reject)=>{const port=connect();let answered=false;const timeout=setTimeout(()=>{port.disconnect();reject(Error("bounded synthetic Host timeout"));},15000);port.onMessage.addListener((r)=>{answered=true;clearTimeout(timeout);resolve(r);port.disconnect();});port.onDisconnect.addListener(()=>{if(!answered){clearTimeout(timeout);reject(Error("synthetic Host exited"));}});port.postMessage(message);});
  const raw=new Uint8Array([77,73,77,69,0,255]);const url="https://accounts.google.com/signin/PAGE_CANARY_PRIVATE";
  const save=(url,title)=>captureAndUploadPage({saveId:crypto.randomUUID(),metadata:{url,title,capturedAt:1,incognito:false},capture:async()=>new Blob([raw]),verify:async()=>{},connect,timeoutMs:15000});
  let panel,server;
  try{
    assert.equal((await call({type:"vault_create",requestId:"c",useDefault:true})).ok,true);
    for(const title of ["PAGE_CANARY_OLD","PAGE_CANARY_NEW"])await save(url,title);
    await save("https://public.test/PAGE_CANARY_PUBLIC","PAGE_CANARY_PUBLIC");
    const reader=createPageClient({connect,runtime:{},timeoutMs:15000});const listed=await reader.list("");assert.equal(listed.items.length,2);assert.equal(listed.items.find((r)=>r.url===url).eventCount,2);assert.equal(listed.items[0].sha256,createHash("sha256").update(raw).digest("hex"));assert.equal(typeof listed.items[0].id,"string");
    // Slice 3 wiring in the real Host, only on paths that end before the OS Save dialog (which a
    // test cannot answer): the request must name the event by id, save id and SHA-256.
    const bounded=(promise)=>Promise.race([promise,new Promise((_,reject)=>setTimeout(()=>reject(Error("bounded synthetic export refusal")),15000).unref())]);
    const row=listed.items[0];
    await assert.rejects(bounded(reader.export({...row,sha256:"0".repeat(64)})),{code:"page_export_changed"});
    await assert.rejects(bounded(reader.export({...row,saveId:crypto.randomUUID()})),{code:"page_export_changed"});
    await assert.rejects(bounded(reader.export({...row,id:"999999"})),{code:"page_not_found"});
    await assert.rejects(bounded(reader.export({...row,id:"0"})),{code:"invalid_target"});
    assert.equal((await reader.list("")).items.length,2);reader.close();
    await confirmPublication(call);server=startMcp(mcpBin,[],{env});await server.initialize("page-slice2-synthetic");
    const response=await server.request("tools/call",{name:"memocampus_search_visits",arguments:{query:""}});assert.ok(!JSON.stringify(response).includes("PAGE_CANARY"));
    const doc=createDocument();doc.querySelector("#panel-page").append(doc.querySelector("#page-groups"),doc.querySelector("#page-delete-dialog"));
    panel=createPagePanel({connect,runtime:{},document:doc,isActive:()=>true,openUrl:()=>{},setTimer:()=>null,clearTimer:()=>{}});panel.activate();
    const wait=async(condition)=>{const deadline=Date.now()+15000;while(!condition()){assert.ok(Date.now()<deadline,"bounded Page UI state");await settle(10);}};
    const groups=()=>doc.querySelector("#page-groups").children;
    await wait(()=>groups().length===2);
    groups().find((g)=>g.children[1].textContent===url).children[0].children[1].dispatch("click");
    await wait(()=>doc.querySelector("#page-delete-dialog").hidden===false);assert.match(doc.querySelector("#page-delete-description").textContent,/保存イベント 2件/);assert.match(doc.querySelector("#page-delete-description").textContent,/共有原本保持 1件/);
    doc.querySelector("#page-delete-confirm").dispatch("click");await wait(()=>groups().length===1&&doc.querySelector("#page-status").textContent.includes("削除しました"));
    doc.querySelector("#page-delete-filter").dispatch("click");await wait(()=>doc.querySelector("#page-delete-dialog").hidden===false);doc.querySelector("#page-delete-confirm").dispatch("click");await wait(()=>groups().length===0&&doc.querySelector("#page-status").textContent.includes("削除しました"));
    const status=await call({type:"vault_status",requestId:"s"});assert.equal(status.status.counts.pages,0);assert.equal(status.status.counts.pageBlobs,0);assert.equal(status.status.counts.visits,0);
  }finally{
    panel?.dispose();await server?.close();
    await Promise.all([...children].map((child)=>new Promise((resolve)=>{child.once("close",resolve);child.kill();})));
    rmSync(app,{recursive:true,force:true});
  }
});
