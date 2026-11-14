import { createDocument } from "./fake-dom.js";
import { createPagePanel } from "../../extension/page-panel.js";
export const pageRow=(id,url=`https://synthetic.test/${id}`,title=`Page ${id}`)=>({id,saveId:`synthetic-save-${id}`,blobId:id,url,title,capturedAt:1000+id,savedAt:1000+id,sha256:String(id).padStart(64,"0"),byteLength:1024,eventCount:1,matchedCount:1});
const event=()=>{const listeners=[];return{addListener:(fn)=>listeners.push(fn),emit:(...args)=>listeners.forEach((fn)=>fn(...args))};};
const fold=(s)=>s.replace(/[A-Z]/g,(c)=>c.toLowerCase());
export function fakePageHost(rows=[]){
  const host={rows:[...rows],requests:[],token:"a".repeat(64),generation:"generation-A",failure:null,delays:{},ports:[],pageSize:2,exports:[],
    // The Host's OS Save dialog: what the user answers, after however long. Default: cancel.
    dialog:async()=>({cancelled:true})};
  host.connect=()=>{
    const context={cursors:new Map(),reviews:new Map(),receipt:null};let closed=false;
    const port={onMessage:event(),onDisconnect:event(),disconnect(){if(closed)return;closed=true;port.onDisconnect.emit();},postMessage(message){
      host.requests.push(structuredClone(message));
      void(async()=>{
        if(host.delays[message.type])await host.delays[message.type](message);
        if(closed)return;
        const envelope={type:`${message.type}_result`,requestId:message.requestId,vaultToken:host.token,generation:host.generation,ok:true};
        if(host.failure){port.onMessage.emit({...envelope,ok:false,error:host.failure});return;}
        if(message.type!=="page_state"&&(message.vaultToken!==host.token||message.generation!==host.generation)){port.onMessage.emit({...envelope,ok:false,error:"vault_switched"});return;}
        const matches=(r,q)=>!q||fold(r.url).includes(fold(q))||fold(r.title).includes(fold(q));
        const sorted=[...host.rows].sort((a,b)=>b.savedAt-a.savedAt||b.id-a.id);
        let result={};
        if(message.type==="page_list"||message.type==="page_history"){
          const query=message.query??"",history=message.type==="page_history";
          const seen=new Set();let items=history?sorted.filter((r)=>r.url===message.url&&matches(r,query)):sorted.filter((r)=>{if(seen.has(r.url))return false;seen.add(r.url);return host.rows.some((s)=>s.url===r.url&&matches(s,query));});
          const cursor=message.cursor&&context.cursors.get(message.cursor);
          if(message.cursor&&!cursor){port.onMessage.emit({...envelope,ok:false,error:"invalid_cursor"});return;}
          if(cursor&&cursor.snapshot!==JSON.stringify(host.rows)){port.onMessage.emit({...envelope,ok:false,error:"page_list_changed"});return;}
          const offset=cursor?.offset??0,hasMore=items.length>offset+host.pageSize;
          items=items.slice(offset,offset+host.pageSize).map((r)=>({...r,eventCount:host.rows.filter((s)=>s.url===r.url).length,matchedCount:host.rows.filter((s)=>s.url===r.url&&matches(s,query)).length}));
          let next=null;if(hasMore){next=crypto.randomUUID();context.cursors.set(next,{offset:offset+host.pageSize,snapshot:JSON.stringify(host.rows)});}result={items,next};
        }
        if(message.type==="page_delete_preview"){
          const target=message.target;
          const selected=host.rows.filter((r)=>target.scope==="event"?r.id===target.id:target.scope==="url"?r.url===target.url:matches(r,target.query));
          if(!selected.length){port.onMessage.emit({...envelope,ok:false,error:"page_not_found"});return;}
          const reviewId=crypto.randomUUID();context.reviews.clear();context.reviews.set(reviewId,{snapshot:JSON.stringify(host.rows),ids:selected.map((r)=>r.id)});
          const blobs=new Set(selected.map((r)=>r.blobId)),gone=[...blobs].filter((id)=>!host.rows.some((r)=>r.blobId===id&&!selected.includes(r)));
          result={reviewId,target,event:target.scope==="event"?selected[0]:null,counts:{events:selected.length,urls:new Set(selected.map((r)=>r.url)).size,blobsRemoved:gone.length,blobsKept:blobs.size-gone.length,bytesRemoved:gone.length*1024}};
        }
        if(message.type==="page_delete"){
          const review=context.reviews.get(message.reviewId);context.reviews.clear();
          if(!review||review.snapshot!==JSON.stringify(host.rows)){port.onMessage.emit({...envelope,ok:false,error:"page_targets_changed"});return;}
          host.rows=host.rows.filter((r)=>!review.ids.includes(r.id));result={operationId:message.reviewId,deleted:{events:review.ids.length}};context.receipt=result;
        }
        if(message.type==="page_export"){
          const row=host.rows.find((r)=>r.id===message.id);
          if(!row){port.onMessage.emit({...envelope,ok:false,error:"page_not_found"});return;}
          if(row.saveId!==message.saveId||row.sha256!==message.sha256){port.onMessage.emit({...envelope,ok:false,error:"page_export_changed"});return;}
          const choice=await host.dialog(message,row);
          if(closed)return;
          // Re-read after the dialog, like the Host: a selection change in between is refused.
          Object.assign(envelope,{vaultToken:host.token,generation:host.generation});
          if(message.vaultToken!==host.token||message.generation!==host.generation){port.onMessage.emit({...envelope,ok:false,error:"vault_switched"});return;}
          if(choice.error){port.onMessage.emit({...envelope,ok:false,error:choice.error});return;}
          if(choice.cancelled)result={id:message.id,cancelled:true};
          else{host.exports.push({id:row.id,fileName:choice.fileName});result={id:message.id,exported:true,fileName:choice.fileName,sha256:row.sha256,byteLength:row.byteLength,previousFileLeft:choice.previousFileLeft===true,...choice.override};}
        }
        if(message.type==="page_delete_status")result=context.receipt?{...context.receipt,confirmed:true}:{confirmed:false};
        port.onMessage.emit({...envelope,...result});
      })();
    }};host.ports.push(port);return port;
  };return host;
}
export function panelFixture(rows=[]){
  const doc=createDocument(),host=fakePageHost(rows),opened=[],timers=new Set();let active=true;
  doc.querySelector("#panel-page").append(doc.querySelector("#page-groups"),doc.querySelector("#page-delete-dialog"));
  const panel=createPagePanel({connect:host.connect,runtime:{},document:doc,isActive:()=>active,openUrl:(url)=>opened.push(url),setTimer:(fn,ms)=>{const timer={fn,ms,unref(){}};timers.add(timer);return timer;},clearTimer:(timer)=>timers.delete(timer)});
  return{doc,$:(id)=>doc.querySelector(`#${id}`),host,panel,opened,tick(ms){for(const timer of [...timers])if(timer.ms===ms){timers.delete(timer);timer.fn();}},pendingTimers:(ms)=>[...timers].filter((t)=>t.ms===ms).length,setActive:(v)=>{active=v;}};
}
export const flatten=(el)=>[el.textContent,...el.children.flatMap((child)=>flatten(child))].join(" ");
