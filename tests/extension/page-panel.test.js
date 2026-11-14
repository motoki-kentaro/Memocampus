import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { panelFixture,pageRow,flatten } from "./page-fixtures.js";
import { settle,loadSidePanel } from "./fake-dom.js";
const loaded=async(rows=[])=>{const f=panelFixture(rows);f.panel.activate();await settle(10);return f;};
const groups=(f)=>f.$("page-groups").children;
const expand=(group)=>group.children[0].children[0];
const groupDelete=(group)=>group.children[0].children[1];
const history=(group)=>group.children.at(-1).children[0];
const requests=(f,type)=>f.host.requests.filter((r)=>r.type===type);

test("Page is lazy, groups exact URLs, uses latest title and never renders source HTML",async()=>{
  const f=panelFixture([pageRow(1,"https://synthetic.test/?q=1","old"),pageRow(2,"https://synthetic.test/?q=1","<img onerror=SECRET>"),pageRow(3,"https://synthetic.test/?q=2","<img onerror=SECRET>")]);
  assert.equal(f.host.requests.length,0);f.panel.activate();await settle(10);
  assert.equal(groups(f).length,2);assert.equal(expand(groups(f)[1]).textContent,"<img onerror=SECRET>");assert.match(flatten(groups(f)[1]),/全2件/);
  const tags=(el)=>[el.tagName,...el.children.flatMap(tags)];assert.ok(!tags(f.$("page-groups")).includes("IMG"));assert.ok(!tags(f.$("page-groups")).includes("SCRIPT"));f.panel.dispose();
});
test("opening original URL is explicit, exact and HTTP(S) only",async()=>{
  const exact="https://synthetic.test/path?q=%E6%97%A5#secret";const f=await loaded([pageRow(1,exact)]);assert.deepEqual(f.opened,[]);
  groups(f)[0].children[3].dispatch("click");assert.deepEqual(f.opened,[exact]);assert.match(groups(f)[0].children[3].title,/保存済みMHTMLの表示ではありません/);f.panel.dispose();
  const invalid=await loaded([pageRow(1,"javascript:alert(1)")]);groups(invalid)[0].children[3].dispatch("click");assert.deepEqual(invalid.opened,[]);invalid.panel.dispose();
});
test("history pages independently from URL groups, expand/collapse keeps loaded history",async()=>{
  const url="https://synthetic.test/shared";const f=await loaded([pageRow(1,url),pageRow(2,url),pageRow(3,url),pageRow(4,"https://other.test/")]);
  const group=groups(f)[1];expand(group).dispatch("click");await settle(5);assert.equal(history(group).children.length,2);assert.equal(requests(f,"page_history")[0].url,url);
  group.children.at(-1).children[1].dispatch("click");await settle(5);assert.equal(history(group).children.length,3);
  expand(group).dispatch("click");expand(group).dispatch("click");await settle(5);assert.equal(requests(f,"page_history").length,2);f.panel.dispose();
});
test("group paging, duplicate-click guard and stable append",async()=>{
  const f=await loaded(Array.from({length:5},(_,i)=>pageRow(i+1)));assert.equal(groups(f).length,2);
  f.$("page-more").dispatch("click");f.$("page-more").dispatch("click");await settle(5);assert.equal(groups(f).length,4);assert.equal(requests(f,"page_list").length,2);
  f.$("page-more").dispatch("click");await settle(5);assert.equal(groups(f).length,5);assert.equal(f.$("page-more").hidden,true);f.panel.dispose();
});
test("literal filter keeps text untrimmed, clears selection, and changes bulk label",async()=>{
  const f=await loaded([pageRow(1,"https://synthetic.test/","prefix %_\\ 日本語 suffix"),pageRow(2)]);
  groupDelete(groups(f)[0]).dispatch("click");await settle(5);assert.equal(f.$("page-delete-dialog").hidden,false);
  f.$("page-query").value=" %_\\ 日本語 ";f.$("page-query").dispatch("input");assert.equal(f.$("page-delete-dialog").hidden,true);assert.equal(groups(f).length,0);
  f.tick(200);await settle(5);assert.equal(requests(f,"page_list").at(-1).query," %_\\ 日本語 ");assert.equal(groups(f).length,1);assert.match(f.$("page-delete-filter").textContent,/条件一致/);f.panel.dispose();
});
test("empty and locked states are distinct and cannot delete",async()=>{
  const f=await loaded();assert.match(f.$("page-status").textContent,/ありません/);assert.equal(f.$("page-delete-filter").disabled,true);
  f.host.failure="vault_locked";f.$("page-refresh").dispatch("click");await settle(5);assert.match(f.$("page-error").textContent,/ロック/);assert.equal(groups(f).length,0);f.panel.dispose();
});
test("individual confirmation shows event date/title/URL, cancellation and keyboard restore focus",async()=>{
  const f=await loaded([pageRow(1)]);const group=groups(f)[0];expand(group).dispatch("click");await settle(5);
  const remove=history(group).children[0].children.at(-1);remove.dispatch("click");await settle(5);
  assert.match(f.$("page-delete-description").textContent,/個別履歴/);assert.match(f.$("page-delete-description").textContent,/Page 1/);assert.match(f.$("page-delete-description").textContent,/元に戻せません/);
  assert.equal(f.doc.activeElement,f.$("page-delete-cancel"));f.$("page-delete-dialog").dispatch("keydown",{key:"Tab"});assert.equal(f.doc.activeElement,f.$("page-delete-confirm"));
  f.$("page-delete-dialog").dispatch("keydown",{key:"Escape"});assert.equal(f.$("page-delete-dialog").hidden,true);assert.equal(f.doc.activeElement,remove);assert.equal(requests(f,"page_delete").length,0);f.panel.dispose();
});
test("URL confirmation removes all history, not just a loaded snapshot",async()=>{
  const url="https://synthetic.test/same";const f=await loaded([pageRow(1,url),pageRow(2,url),pageRow(3,url)]);
  groupDelete(groups(f)[0]).dispatch("click");await settle(5);assert.match(f.$("page-delete-description").textContent,/URL全体/);assert.match(f.$("page-delete-description").textContent,/保存イベント 3件/);
  f.$("page-delete-confirm").dispatch("click");await settle(10);assert.equal(f.host.rows.length,0);assert.equal(groups(f).length,0);assert.match(f.$("page-status").textContent,/削除しました/);f.panel.dispose();
});
test("unfiltered bulk explicitly confirms ALL database events beyond loaded groups",async()=>{
  const f=await loaded(Array.from({length:6},(_,i)=>pageRow(i+1)));assert.equal(groups(f).length,2);
  f.$("page-delete-filter").dispatch("click");await settle(5);assert.match(f.$("page-delete-description").textContent,/全Page/);assert.match(f.$("page-delete-description").textContent,/保存イベント 6件/);
  f.$("page-delete-confirm").dispatch("click");await settle(10);assert.equal(f.host.rows.length,0);f.panel.dispose();
});
test("filtered bulk confirms exact filter and all matches; keeps nonmatching rows",async()=>{
  const f=await loaded([pageRow(1,undefined,"needle"),pageRow(2),pageRow(3,undefined,"needle")]);f.$("page-query").value="needle";f.$("page-query").dispatch("input");f.tick(200);await settle(5);
  f.$("page-delete-filter").dispatch("click");await settle(5);assert.match(f.$("page-delete-description").textContent,/検索条件: needle/);assert.match(f.$("page-delete-description").textContent,/DB上の一致全件/);
  f.$("page-delete-confirm").dispatch("click");await settle(10);assert.deepEqual(f.host.rows.map((r)=>r.id),[2]);f.panel.dispose();
});
test("changed target rejects without optimistic deletion",async()=>{
  const f=await loaded([pageRow(1)]);groupDelete(groups(f)[0]).dispatch("click");await settle(5);f.host.rows.push(pageRow(2,f.host.rows[0].url));
  f.$("page-delete-confirm").dispatch("click");await settle(5);assert.equal(f.host.rows.length,2);assert.equal(groups(f).length,1);assert.match(f.$("page-error").textContent,/変更されました/);assert.equal(f.$("page-delete-dialog").hidden,true);f.panel.dispose();
});
test("duplicate confirmation sends once and keeps UI until COMMIT reply",async()=>{
  const f=await loaded([pageRow(1)]);groupDelete(groups(f)[0]).dispatch("click");await settle(5);let release;f.host.delays.page_delete=()=>new Promise((r)=>{release=r;});
  f.$("page-delete-confirm").dispatch("click");f.$("page-delete-confirm").dispatch("click");await settle(5);assert.equal(requests(f,"page_delete").length,1);assert.equal(groups(f).length,1);assert.equal(f.$("page-query").disabled,true);
  release();await settle(10);assert.equal(groups(f).length,0);f.panel.dispose();
});
test("Vault switch discards old list and confirmation replies",async()=>{
  const f=panelFixture([pageRow(1)]);let release;f.host.delays.page_list=()=>new Promise((r)=>{release=r;});f.panel.activate();await settle(5);
  f.setActive(false);f.panel.vaultChanged();f.host.rows=[pageRow(2)];release();await settle(5);assert.equal(groups(f).length,0);
  delete f.host.delays.page_list;f.setActive(true);f.panel.activate();await settle(5);assert.equal(groups(f).length,1);assert.match(flatten(groups(f)[0]),/Page 2/);
  f.host.delays.page_delete_preview=()=>new Promise((r)=>{release=r;});groupDelete(groups(f)[0]).dispatch("click");await settle(5);f.setActive(false);f.panel.vaultChanged();release();await settle(5);assert.equal(f.$("page-delete-dialog").hidden,true);f.panel.dispose();
});
test("poll invalidates open confirmation across A→B→A generation even with same file token",async()=>{
  const f=await loaded([pageRow(1)]);groupDelete(groups(f)[0]).dispatch("click");await settle(5);
  f.host.generation="generation-A-after-B";f.tick(5000);await settle(5);assert.equal(groups(f).length,0);assert.equal(f.$("page-delete-dialog").hidden,true);assert.match(f.$("page-error").textContent,/Vaultが変更/);f.panel.dispose();
});
test("new saves refresh safe top view but only mark dot while history is expanded",async()=>{
  const f=await loaded([pageRow(1)]);f.host.rows.push(pageRow(2));f.panel.saved({ok:true,vaultToken:f.host.token,generation:f.host.generation});await settle(5);assert.equal(groups(f).length,2);assert.equal(f.$("page-new-dot").hidden,true);
  expand(groups(f)[0]).dispatch("click");await settle(5);const before=requests(f,"page_list").length;f.host.rows.push(pageRow(3));f.panel.saved({ok:true,vaultToken:f.host.token,generation:f.host.generation});await settle(5);
  assert.equal(requests(f,"page_list").length,before);assert.equal(f.$("page-new-dot").hidden,false);assert.match(f.$("tab-page").getAttribute("aria-label"),/新しい保存/);f.panel.dispose();
});
test("Page markup has native modal semantics, safe text rendering and no preview or in-browser file handling",()=>{
  const html=readFileSync(new URL("../../extension/sidepanel.html",import.meta.url),"utf8");assert.match(html,/<dialog id="page-delete-dialog"[^>]+aria-modal="true"/);
  const source=readFileSync(new URL("../../extension/page-panel.js",import.meta.url),"utf8");assert.doesNotMatch(source,/innerHTML|insertAdjacentHTML|page_read|eval\(/);
  // Export is a request to the Host only: no bytes, object URLs, downloads or pickers in the extension.
  for(const file of ["page-panel.js","page-client.js","page-text.js"])assert.doesNotMatch(readFileSync(new URL(`../../extension/${file}`,import.meta.url),"utf8"),/createObjectURL|FileReader|showSaveFilePicker|chrome\.downloads|atob\(|<iframe|srcdoc/);
  const manifest=JSON.parse(readFileSync(new URL("../../extension/manifest.json",import.meta.url),"utf8"));
  assert.deepEqual([...manifest.permissions].sort(),["contextMenus","nativeMessaging","notifications","pageCapture","scripting","sidePanel","storage","tabs","webNavigation"]);
});
test("Vault change during post-delete refresh cannot show stale success or steal focus",async()=>{
  const f=await loaded([pageRow(1)]);groupDelete(groups(f)[0]).dispatch("click");await settle(5);
  let release;f.host.delays.page_list=()=>new Promise((r)=>{release=r;});f.$("page-delete-confirm").dispatch("click");await settle(5);assert.equal(typeof release,"function");
  f.setActive(false);f.panel.vaultChanged();release();await settle(5);assert.doesNotMatch(f.$("page-status").textContent,/削除しました/);assert.equal(groups(f).length,0);f.panel.dispose();
});
test("activation during a pending poll cannot duplicate timers, and dispose cannot restart polling",async()=>{
  const f=await loaded([pageRow(1)]);let release;f.host.delays.page_state=()=>new Promise((r)=>{release=r;});f.tick(5000);await settle(5);f.panel.activate();assert.equal(f.pendingTimers(5000),0);
  f.panel.dispose();release();await settle(5);assert.equal(f.pendingTimers(5000),0);
});
test("real Side Panel routes Page activation while preserving URL/Image tab state",async()=>{
  const f=await loadSidePanel({active:"A"});await settle(20);let connected=0;
  const fixture=panelFixture([pageRow(1)]);globalThis.chrome.runtime.connectNative=()=>{connected++;return fixture.host.connect();};
  f.$("#query").value="URL draft";f.$("#tab-page").dispatch("click");await settle(10);assert.equal(connected,1);assert.equal(f.$("#panel-page").hidden,false);assert.equal(f.$("#panel-url").hidden,true);
  f.$("#tab-url").dispatch("click");assert.equal(f.$("#query").value,"URL draft");assert.equal(f.$("#panel-page").hidden,true);fixture.panel.dispose();
});
const eventsOf=async(f,index=0)=>{const group=groups(f)[index];expand(group).dispatch("click");await settle(5);return history(group).children;};
const exportButton=(li)=>li.children.at(-2);
const dialogGate=(f,answer)=>{let release;f.host.dialog=()=>new Promise((resolve)=>{release=()=>resolve(answer);});return()=>release();};
test("each history row exports exactly its own save event; everything waits while the OS dialog is open",async()=>{
  const url="https://synthetic.test/same";const f=await loaded([pageRow(1,url,"old"),pageRow(2,url,"new"),pageRow(3,"https://other.test/")]);
  const rows=await eventsOf(f,1);assert.equal(rows.length,2);
  const older=rows[1],button=exportButton(older);assert.equal(button.textContent,"MHTMLを書き出す");assert.notEqual(button,groups(f)[1].children[3]);assert.match(button.title,/保存ダイアログ/);
  assert.equal(older.children.at(-1).textContent,"この保存履歴を削除");
  const release=dialogGate(f,{fileName:"<img src=x onerror=1>.mhtml"});const before=f.host.requests.length;
  button.focus();button.dispatch("click");button.dispatch("click");exportButton(rows[0]).dispatch("click");await settle(5);
  const sent=requests(f,"page_export");assert.equal(sent.length,1);assert.deepEqual([sent[0].id,sent[0].saveId,sent[0].sha256],[1,"synthetic-save-1","1".padStart(64,"0")]);
  assert.match(f.$("page-status").textContent,/保存ダイアログ/);assert.equal(f.$("panel-page").getAttribute("aria-busy"),"true");
  for(const id of ["page-query","page-refresh","page-more","page-delete-filter"])assert.equal(f.$(id).disabled,true,id);
  for(const li of rows)for(const control of [exportButton(li),li.children.at(-1)])assert.equal(control.disabled,true);
  assert.equal(groupDelete(groups(f)[0]).disabled,true);
  // Nothing else is sent to the busy Host: no poll, refresh, delete review, history or new-save reload.
  f.tick(5000);f.$("page-refresh").dispatch("click");groupDelete(groups(f)[0]).dispatch("click");expand(groups(f)[0]).dispatch("click");f.panel.saved({ok:true,vaultToken:f.host.token,generation:f.host.generation});await settle(5);
  assert.equal(f.host.requests.length,before+1);assert.equal(f.$("page-delete-dialog").hidden,true);
  release();await settle(10);
  assert.equal(f.$("page-status").textContent,"「<img src=x onerror=1>.mhtml」にMHTML原本を書き出しました。");assert.equal(f.$("page-error").hidden,true);
  const tags=(el)=>[el.tagName,...el.children.flatMap(tags)];assert.ok(!tags(f.$("panel-page")).includes("IMG"));
  assert.equal(button.disabled,false);assert.equal(f.$("page-query").disabled,false);assert.equal(f.doc.activeElement,button);assert.equal(f.$("panel-page").getAttribute("aria-busy"),"false");
  // The history opened meanwhile loads now; rows and the URL group are still there (nothing was deleted).
  assert.equal(history(groups(f)[0]).children.length,1);assert.equal(f.host.rows.length,3);assert.deepEqual(f.host.exports,[{id:1,fileName:"<img src=x onerror=1>.mhtml"}]);
  // A second, separate click is a second, separate export.
  f.host.dialog=async()=>({fileName:"b.mhtml",previousFileLeft:true});exportButton(rows[0]).dispatch("click");await settle(10);
  assert.equal(requests(f,"page_export").at(-1).id,2);assert.match(f.$("page-status").textContent,/「b\.mhtml」.*\.memocampus-old/);f.panel.dispose();
});
test("export cancel is quiet, failure and unknown are distinct, and none claims success",async()=>{
  const f=await loaded([pageRow(1)]);const rows=await eventsOf(f);const button=exportButton(rows[0]);
  button.dispatch("click");await settle(10);assert.match(f.$("page-status").textContent,/キャンセル/);assert.equal(f.$("page-error").hidden,true);assert.equal(button.disabled,false);
  for(const [error,text] of [["export_file_appeared",/上書きせず/],["export_target_busy",/使用中/],["page_export_changed",/変更・削除/],["page_blob_corrupt",/検証に失敗/],["export_verify_failed",/取り消しました/],["export_recovery_needed",/memocampus-old/],["parent_missing",/フォルダー/],["some_new_code",/失敗しました/]]){
    f.host.dialog=async()=>({error});button.dispatch("click");await settle(10);
    assert.match(f.$("page-error").textContent,text,error);assert.equal(f.$("page-error").hidden,false);assert.equal(f.$("page-status").textContent,"");assert.equal(button.disabled,false);
  }
  assert.equal(f.host.exports.length,0);
  // The Host goes away with the dialog open: the file may or may not exist. Not resent.
  f.host.dialog=async()=>{f.host.ports.at(-1).disconnect();return{fileName:"x.mhtml"};};const sent=requests(f,"page_export").length;
  button.dispatch("click");await settle(10);assert.equal(requests(f,"page_export").length,sent+1);
  assert.doesNotMatch(f.$("page-status").textContent,/書き出しました/);assert.match(f.$("page-error").textContent,/未確定|接続できません/);f.panel.dispose();
});
test("Vault change while the export dialog is open drops the old answer and reloads the new Vault",async()=>{
  const f=await loaded([pageRow(1)]);const rows=await eventsOf(f);const release=dialogGate(f,{fileName:"old-vault.mhtml"});
  exportButton(rows[0]).dispatch("click");await settle(5);
  f.host.rows=[pageRow(7)];f.host.generation="generation-B";f.panel.vaultChanged();await settle(10);
  assert.equal(groups(f).length,1);assert.match(flatten(groups(f)[0]),/Page 7/);assert.equal(f.host.ports.length,2);
  release();await settle(10);assert.doesNotMatch(f.$("page-status").textContent,/書き出しました|キャンセル/);assert.equal(f.host.exports.length,0);assert.equal(requests(f,"page_export").length,1);
  // The new Vault's rows export normally afterwards.
  f.host.dialog=async()=>({fileName:"new.mhtml"});exportButton((await eventsOf(f))[0]).dispatch("click");await settle(10);assert.match(f.$("page-status").textContent,/「new\.mhtml」/);f.panel.dispose();
});
test("a selection change noticed by the Host after the dialog invalidates the view (A→B→A)",async()=>{
  const f=await loaded([pageRow(1)]);const rows=await eventsOf(f);
  f.host.dialog=async()=>{f.host.generation="generation-A-after-B";return{fileName:"x.mhtml"};};
  exportButton(rows[0]).dispatch("click");await settle(10);
  assert.match(f.$("page-error").textContent,/Vaultが変更/);assert.equal(groups(f).length,0);assert.doesNotMatch(f.$("page-status").textContent,/書き出しました/);assert.equal(f.host.exports.length,0);f.panel.dispose();
});
