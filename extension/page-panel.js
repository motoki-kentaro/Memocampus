import { createPageClient } from "./page-client.js";
import { PAGE_TEXT as T, pageErrorText, pageDate, pageBytes, historyLabel, summaryText, deletionText, exportedText } from "./page-text.js";
import { isWebUrl } from "./search-client.js";

export function createPagePanel({ connect, runtime, isActive, openUrl, document: doc = document, setTimer = setTimeout, clearTimer = clearTimeout }) {
  const $ = (id) => doc.querySelector(`#${id}`);
  const panel=$("panel-page"), input=$("page-query"), list=$("page-groups"), status=$("page-status"), error=$("page-error"), more=$("page-more"), refresh=$("page-refresh"), bulk=$("page-delete-filter"), dot=$("page-new-dot"), dialog=$("page-delete-dialog"), confirm=$("page-delete-confirm"), cancel=$("page-delete-cancel");
  input.placeholder=T.search; input.setAttribute("aria-label",T.search);
  $("page-note").textContent=T.scopeNote; $("page-open-note").textContent=T.openNote;
  $("page-delete-title").textContent=T.confirmTitle; confirm.textContent=T.confirm; cancel.textContent=T.cancel;
  refresh.textContent=T.refresh; more.textContent=T.more; bulk.textContent=T.all; status.textContent=T.initial;
  let epoch=0, query="", cursor=null, loaded=false, loading=false, deleting=false, reviewing=false, review=null, timer=null, searchTimer=null, trigger=null;
  let polling=false, disposed=false, exporting=false;
  const groups=new Map();
  const client=createPageClient({ connect, runtime, onInvalidated: (code) => invalidate(code) });
  function buttons() {
    // While the Host shows its Save dialog it answers nothing else: every Page request waits.
    input.disabled=deleting||exporting;
    bulk.textContent=query===""?T.all:T.bulk;
    refresh.disabled=loading||deleting||exporting; more.disabled=loading||deleting||exporting;
    bulk.disabled=loading||deleting||reviewing||exporting||!loaded||groups.size===0;
    confirm.disabled=!review||deleting; cancel.disabled=deleting;
    confirm.textContent=deleting?T.deleting:T.confirm;
    dialog.setAttribute("aria-busy",String(deleting));
    for (const group of groups.values()) {
      for (const button of [...group.deletions,...group.exports]) button.disabled=loading||deleting||reviewing||exporting;
      group.more.disabled=group.busy||exporting;
    }
    panel.setAttribute("aria-busy",String(loading||deleting||reviewing||exporting));
  }
  function closeReview(restore=true) {
    review=null; reviewing=false; if(dialog.open)dialog.close();dialog.hidden=true; confirm.disabled=true;
    if (restore && trigger && panel.contains(trigger)) trigger.focus(); trigger=null;
  }
  function clear() {
    epoch++; loaded=false; loading=false; exporting=false; cursor=null; groups.clear(); list.replaceChildren(); more.hidden=true;
    closeReview(false); buttons();
  }
  function invalidate(code) {
    if(disposed)return;
    clear();
    error.textContent=pageErrorText(code);
    error.hidden=false;
    status.textContent="";
  }
  function fail(err) { error.textContent=pageErrorText(err.code);error.hidden=false; }
  function node(tag,text,cls="") { const el=doc.createElement(tag);el.textContent=text;el.className=cls;return el; }
  function button(text,action) { const b=node("button",text,"secondary");b.type="button";b.addEventListener("click",action);return b; }
  async function ask(target,origin) {
    if (deleting||reviewing||loading||exporting) return;
    closeReview(false); reviewing=true; trigger=origin; error.hidden=true; buttons();
    const version=epoch, filter=query;
    try {
      const response=await client.preview(target);
      if (version!==epoch||query!==filter) return;
      review=response;$("page-delete-description").textContent=deletionText(response);dialog.hidden=false;dialog.showModal?.();buttons();cancel.focus();
    } catch(err) { if(version===epoch){reviewing=false;fail(err);buttons();} }
  }
  // One explicit click, one request for exactly this save event (id + save id + SHA-256). The
  // list, filter and history shown later cannot change what was asked for.
  async function exportEvent(item,origin) {
    if (exporting||deleting||reviewing||loading) return;
    const version=epoch, target={id:item.id,saveId:item.saveId,sha256:item.sha256};
    exporting=true; error.hidden=true; status.textContent=T.exporting; buttons();
    try {
      const result=await client.export(target);
      if (version!==epoch) return;
      status.textContent=result.cancelled?T.exportCancelled:exportedText(result);
    } catch(err) { if(version===epoch){fail(err);status.textContent="";} }
    finally {
      if (version===epoch) {
        exporting=false; buttons();
        // The button was disabled meanwhile; give the focus back unless the user moved on.
        if (isActive()&&panel.contains(origin)&&(!doc.activeElement||doc.activeElement===doc.body||doc.activeElement===origin)) origin.focus();
        for (const group of groups.values()) if(!group.body.hidden&&!group.loaded) void history(group);
      }
    }
  }
  function appendEvents(group,items) {
    for (const item of items) {
      const li=node("li","","page-event");
      li.append(node("time",pageDate(item.savedAt)),node("span",item.title||T.untitled,"page-event-title"),node("small",pageBytes(item.byteLength)));
      const save=button(T.eventExport,()=>void exportEvent(item,save)); save.className="secondary page-event-export"; save.title=T.exportNote;
      const remove=button(T.eventDelete,()=>void ask({scope:"event",id:item.id},remove));
      li.append(save,remove); group.exports.push(save); group.deletions.push(remove); group.history.append(li);
    }
  }
  async function history(group,append=false) {
    if(group.busy||deleting||exporting) return;
    group.busy=true;group.more.disabled=true;const version=epoch,filter=query;
    try {
      const response=await client.history(group.url,filter,append?group.cursor:null);
      if(version!==epoch||filter!==query||groups.get(group.url)!==group)return;
      if(!append)group.history.replaceChildren();appendEvents(group,response.items);group.cursor=response.next;group.loaded=true;group.more.hidden=!response.next;
    } catch(err){if(version===epoch){fail(err);}}
    finally{group.busy=false;buttons();}
  }
  function appendGroup(item) {
    if(groups.has(item.url))return;
    const li=node("li","","page-group"), heading=node("div","","page-group-heading");
    const expand=button(item.title||T.untitled,()=>{const open=expand.getAttribute("aria-expanded")!=="true";expand.setAttribute("aria-expanded",String(open));body.hidden=!open;if(open&&!group.loaded)void history(group);});
    expand.className="page-expand";expand.setAttribute("aria-expanded","false");
    const body=node("div","","page-history");body.id=`page-history-${item.id}`;body.hidden=true;expand.setAttribute("aria-controls",body.id);
    const remove=button(T.groupDelete,()=>void ask({scope:"url",url:item.url},remove)); remove.className="secondary page-group-delete";
    const visit=button(T.open,()=>{if(isWebUrl(item.url))openUrl(item.url);});visit.disabled=!isWebUrl(item.url);visit.title=T.openNote;
    const historyList=node("ol","","page-events"), next=button(T.more,()=>void history(group,true));next.hidden=true;
    const group={url:item.url,history:historyList,more:next,cursor:null,loaded:false,busy:false,deletions:[remove],exports:[],body};
    heading.append(expand,remove);li.append(heading,node("p",item.url,"page-url"),node("p",`${pageDate(item.savedAt)} · ${historyLabel(item)}`,"vault-meta"),visit,body);body.append(historyList,next);list.append(li);groups.set(item.url,group);
  }
  async function load(append=false) {
    if(loading||deleting||exporting||disposed)return;
    if(!append){clear(); query=input.value; dot.hidden=true;$("tab-page").setAttribute("aria-label",T.tab);}
    const version=epoch,filter=query; loading=true;error.hidden=true;status.textContent=T.loading;buttons();
    try {
      const response=await client.list(filter,append?cursor:null);
      if(version!==epoch||filter!==query)return;
      for(const item of response.items)appendGroup(item);
      cursor=response.next;
      loaded=true;
      more.hidden=!cursor;
      status.textContent=groups.size?summaryText(groups.size):T.empty;
      return version;
    }catch(err){if(version===epoch){fail(err);status.textContent="";}}
    finally{if(version===epoch){loading=false;buttons();}}
  }
  input.addEventListener("input",()=>{
    clearTimer(searchTimer);closeReview(false);clear();query=input.value;
    searchTimer=setTimer(()=>void load(),200);
  });
  refresh.addEventListener("click",()=>void load());more.addEventListener("click",()=>void load(true));
  bulk.addEventListener("click",()=>void ask({scope:"filter",query},bulk));cancel.addEventListener("click",()=>{closeReview();buttons();});
  dialog.addEventListener("keydown",(event)=>{
    if(event.key==="Escape"&&!deleting){event.preventDefault();closeReview();buttons();}
    if(event.key==="Tab"){event.preventDefault();if(!deleting)(doc.activeElement===cancel?confirm:cancel).focus();}
  });
  dialog.addEventListener("cancel",(event)=>{event.preventDefault();if(!deleting){closeReview();buttons();}});
  confirm.addEventListener("click",async()=>{
    if(!review||deleting)return;
    const current=review,version=epoch;deleting=true;buttons();status.textContent=T.deleting;
    try{
      await client.delete(current.reviewId);
      if(version!==epoch)return;
      closeReview(false);
      deleting=false;
      const refreshed=await load();
      // A switch/filter change during the post-COMMIT refresh invalidates even the success text.
      if(refreshed===epoch){status.textContent=T.deleted;refresh.focus();}
    }catch(err){if(version===epoch){closeReview();fail(err);status.textContent="";}}
    finally{deleting=false;buttons();}
  });
  async function poll() {
    if(disposed||polling)return;
    timer=null;
    polling=true;
    try {
      if(!deleting&&!loading&&!exporting&&client.binding){
        try{await client.state();}
        catch(err){invalidate(err.code);client.close();}
      }
    } finally {
      polling=false;
      if(!disposed){timer=setTimer(()=>void poll(),5000);timer?.unref?.();}
    }
  }
  buttons();
  return {
    activate(){if(disposed)return;if(!timer&&!polling){timer=setTimer(()=>void poll(),5000);timer?.unref?.();}if(!loaded&&!loading)void load();},
    vaultChanged(){clearTimer(searchTimer);client.close();clear();error.hidden=true;status.textContent=T.initial;dot.hidden=true;$("tab-page").setAttribute("aria-label",T.tab);if(isActive())void load();},
    vaultStatus(status){if(client.binding&&status?.state!=="ready"){client.close();invalidate(status?.state==="locked"?"vault_locked":"vault_switched");}},
    saved(message){if(message.ok!==true)return;if(client.binding&&(message.vaultToken!==client.binding.vaultToken||message.generation!==client.binding.generation)){client.close();invalidate("vault_switched");return;}dot.hidden=false;dot.title=T.newItems;$("tab-page").setAttribute("aria-label",`${T.tab}: ${T.newItems}`);if(isActive()&&!loading&&!deleting&&!reviewing&&!exporting&&panel.scrollTop===0&&![...groups.values()].some((g)=>!g.body.hidden)&&!list.contains(doc.activeElement))void load();},
    dispose(){disposed=true;clearTimer(timer);clearTimer(searchTimer);client.close();clear();},
  };
}
