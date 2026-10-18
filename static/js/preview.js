
const params=new URLSearchParams(window.location.search);
const layoutId=params.get('layout');
let previewCountdownTimer=null;
var RIPLEY_COUNTDOWN_MAX_FONT_SIZE=160;
function ripleyCountdownFontSize(value){const size=Number(value);return Math.max(8,Math.min(RIPLEY_COUNTDOWN_MAX_FONT_SIZE,Number.isFinite(size)?Math.round(size):48));}
const previewSoundStates={};
function playPreviewSound(widget){
 const p=widget.props||{}, tracks=Array.isArray(p.playlist)?p.playlist.filter(function(t){return t&&t.url;}):[];
 if(!tracks.length) return;
 let s=previewSoundStates[widget.id];
 if(!s){s={audio:new Audio(),tracks:tracks.slice(),index:0};previewSoundStates[widget.id]=s;s.audio.onended=function(){if(p.replay===false&&s.index>=s.tracks.length-1)return;s.index++;if(s.index>=s.tracks.length){s.index=0;if(p.shuffle)s.tracks.sort(function(){return Math.random()-.5;});}playPreviewSound(widget);};}
 s.audio.src=s.tracks[s.index].url;s.audio.volume=Math.max(0,Math.min(1,Number(p.volume==null ? .7 : p.volume)));s.audio.play().catch(function(e){var msg=document.getElementById('preview-status');if(msg){msg.textContent='Sound playback blocked: click the button again';msg.className='error';}});
}
function fitPreviewCountdownNode(node){
 const value=node.querySelector('.countdown-value'); if(!value||!node.clientWidth||!node.clientHeight)return;
 const requested=ripleyCountdownFontSize(node.dataset.countdownFontSize);
 var effective = typeof ripleyScaledPixels === 'function' ? ripleyScaledPixels(requested) : requested;
  value.style.fontSize=Math.floor(effective)+'px';
 const label=value.previousElementSibling;
 if(label)label.style.fontSize=Math.max(8,Math.min(24,Math.floor(Math.min(node.clientWidth/Math.max(8,label.textContent.length*.55),node.clientHeight*.16))))+'px';
}
function startPreviewCountdowns(layout){
 if(previewCountdownTimer) clearInterval(previewCountdownTimer);
 const nodes=Array.from(document.querySelectorAll('#stage [data-countdown-widget]'));
 if(!nodes.length) return;
 function tick(){
  const now=Date.now();
  nodes.forEach(function(node){
   if(node.dataset.countdownExpired==='1') return;
   const widget=(layout.widgets||[]).find(function(x){return x.id===node.dataset.countdownWidgetId;});
   if(widget&&widget.props&&widget.props.countdown_mode==='duration'){
    const units={seconds:1000,minutes:60000,hours:3600000,days:86400000};
    const duration=Math.max(1,Number(widget.props.duration_value||1))*(units[widget.props.duration_unit||'minutes']||60000);
    if(!node.dataset.countdownStartedAt) node.dataset.countdownStartedAt=String(now);
    node.dataset.countdownTarget=new Date(Number(node.dataset.countdownStartedAt)+duration).toISOString();
   }
   const target=new Date(node.dataset.countdownTarget||''); if(isNaN(target.getTime())) return;
   const diff=target.getTime()-now; const value=node.querySelector('.countdown-value');
   if(value){const d=Math.max(0,diff),days=Math.floor(d/86400000),h=Math.floor(d%86400000/3600000),m=Math.floor(d%3600000/60000),s=Math.floor(d%60000/1000);value.textContent=diff<=0?(node.dataset.countdownExpiryText||''):(days?days+'d '+h+'h':h?h+'h '+m+'m':m?m+'m '+s+'s':s+'s');fitPreviewCountdownNode(node);}
   if(diff>0) return;
   node.dataset.countdownExpired='1';
   const action=node.dataset.countdownAction||'none', targetId=node.dataset.countdownTargetLayout;
   if(action==='submit_page'){
    const w=(layout.widgets||[]).find(function(x){return x.id===node.dataset.countdownWidgetId;})||{id:'countdown',widget_type:'countdown',props:{}};
    submitInteractive(w,{action:'countdown_expired'},node);
   } else if(action==='page_switch' && targetId){
    fetch('/api/layouts/'+encodeURIComponent(targetId)+'/render').then(function(r){return r.ok?r.json():null;}).then(async function(target){if(target){await ripleyHydrateTrafficTemplates(target);document.getElementById('title').textContent='Preview: '+target.name+' • '+target.id;renderShared(target);}});
   }
  });
 }
 tick();
 previewCountdownTimer=setInterval(tick,1000);
}
function preparePreviewVideoPlayback(root){
 const videos=Array.from((root||document).querySelectorAll('video'));
 videos.forEach(function(video){
  // Browsers block autoplay with sound in an editor preview. Mute the
  // preview-only element so the first frame actually appears, while leaving
  // the live player's configured mute setting unchanged.
  video.muted=true;
  video.defaultMuted=true;
  video.autoplay=true;
  video.playsInline=true;
  video.preload='auto';
  video.setAttribute('muted','');
  video.setAttribute('controls','');
  function tryPlay(){
   var attempt=video.play();
   if(attempt && typeof attempt.catch==='function') attempt.catch(function(){});
  }
  video.addEventListener('loadeddata',tryPlay);
  video.addEventListener('canplay',tryPlay);
  tryPlay();
  video.addEventListener('error',function(){
   var msg=document.getElementById('preview-status');
   var detail=video.error && video.error.code ? ' (media error '+video.error.code+')' : '';
   if(msg){msg.textContent='Video failed to load'+detail;msg.className='error';msg.hidden=false;}
  });
 });
 // YouTube uses an iframe rather than a <video> element. Force its preview
 // player to use the same muted-autoplay policy so it is visible immediately.
 Array.from((root||document).querySelectorAll('iframe[src*="youtube.com/embed/"]')).forEach(function(frame){
  try{
   var url=new URL(frame.src,window.location.href);
   url.searchParams.set('autoplay','1');
   url.searchParams.set('mute','1');
   frame.src=url.href;
  }catch(_e){}
 });
}
function renderShared(layout){
 window.__previewLayout=layout;
 ripleySetRenderOrientation(layout);
 document.body.classList.toggle('preview-portrait', layout && layout.canvas_orientation === 'portrait');
 const stage=document.getElementById('stage'); stage.innerHTML=(layout.widgets||[]).slice().sort((a,b)=>(a.z||0)-(b.z||0)).map(function(w){
   // buildWidgetHTML accepts the API's percentage geometry directly. Do not
  // divide it here: values just over 100% (for example 100.376%) would become
  // 1.00376 and then be divided again by the shared renderer, shrinking the
  // widget to roughly one percent in the upper-left corner.
  const copy=Object.assign({},w,{props:Object.assign({},w.props||{})});
   if(copy.widget_type==='text' && copy.props.content!=null) copy.props.text=copy.props.content;
   return window.RipleyPreviewRenderer.buildWidgetHTML(copy);
 }).join('');
 if(typeof ripleyFitTrafficTemplateText==='function') ripleyFitTrafficTemplateText(stage);
 if(typeof initVideoWidgets==='function') initVideoWidgets(layout.widgets||[], stage);
 preparePreviewVideoPlayback(stage);
 if(typeof initImageWidgets==='function') initImageWidgets(layout.widgets||[], stage);
 if(typeof injectInstalledFontsIntoIframes==='function') injectInstalledFontsIntoIframes(stage);
 var keyboardRuntime = window.RipleyPreviewRenderer && window.RipleyPreviewRenderer.bindInteractiveKeyboardRuntime;
 if(typeof keyboardRuntime==='function') keyboardRuntime(stage);
 startPreviewCountdowns(layout);
 stage.querySelectorAll('.interactive-live-button').forEach(function(btn){btn.onclick=async function(){const buttonWidget=(layout.widgets||[]).find(function(x){return x.id===btn.closest('[data-interactive-widget-id]').dataset.interactiveWidgetId;});if(buttonWidget&&buttonWidget.props&&buttonWidget.props.action==='play_sound'){const soundWidget=(layout.widgets||[]).find(function(x){return x.id===buttonWidget.props.targetSoundWidgetId;});if(soundWidget)playPreviewSound(soundWidget);return;}const targetId=btn.dataset.interactiveTargetPage;if(!targetId){return submitInteractive((layout.widgets||[]).find(function(w){return w.id===btn.closest('[data-interactive-widget-id]').dataset.interactiveWidgetId}),{action:'click'},btn);}const r=await fetch('/api/layouts/'+encodeURIComponent(targetId)+'/render');if(!r.ok){document.getElementById('preview-status').textContent='Target layout not found';return;}const target=await r.json();await ripleyHydrateTrafficTemplates(target);await refreshPreviewWeather(target);await refreshPreviewTraffic(target);await refreshPreviewCalendars(target);document.getElementById('title').textContent='Preview: '+target.name+' • '+target.id;renderShared(target);document.getElementById('preview-status').textContent='Switched to layout: '+target.name;};});
 stage.querySelectorAll('[data-interactive-widget-id]').forEach(function(node){const w=(layout.widgets||[]).find(function(x){return x.id===node.dataset.interactiveWidgetId;});if(!w||w.widget_type==='button')return;node.querySelectorAll('input,select,textarea').forEach(function(input){input.addEventListener('change',function(){let payload={};if(w.widget_type==='checkbox')payload.checked=input.checked;else if(w.widget_type==='checkbox-list')payload.checkedValues=Array.from(node.querySelectorAll('input:checked')).map(function(x){return x.value;});else if(w.widget_type==='radio')payload.selected=input.value;else if(w.widget_type==='dropdown')payload.selected=input.value;else payload.value=input.value;submitInteractive(w,payload,node);});});});
}
async function refreshPreviewWeather(layout){
 const widgets=(layout.widgets||[]).filter(function(w){return w&&((w.widget_type||w.type)==='weather')&&w.props&&Number(w.props.lat)&&Number(w.props.lon);});
 await Promise.all(widgets.map(async function(w){try{const r=await fetch('/api/weather/forecast?lat='+encodeURIComponent(w.props.lat)+'&lon='+encodeURIComponent(w.props.lon),{cache:'no-store'});if(r.ok){const fresh=await r.json();w.props=Object.assign({},w.props,{_weather_data:fresh});}}catch(_e){}}));
}
async function refreshPreviewTraffic(layout){
 const widgets=(layout.widgets||[]).filter(function(w){return w&&((w.widget_type||w.type)==='traffic')&&w.props;});
 await Promise.all(widgets.map(async function(w){
  var p=w.props||{};
  if(!Number(p.lat)||!Number(p.lon)){p._traffic_data={configured:false,events:[]};p._traffic_data_source='unavailable';return;}
  try{
   var url='/api/traffic/events?lat='+encodeURIComponent(p.lat)+'&lon='+encodeURIComponent(p.lon)+'&radius_miles='+encodeURIComponent(p.radius_miles||25);
   var r=await fetch(url,{cache:'no-store'});
   if(!r.ok) throw new Error('traffic request failed');
   p._traffic_data=await r.json();
   if(p._traffic_data.configured!==false && p._traffic_data.available!==false && typeof window.ripleyFilterTrafficEvents==='function') p._traffic_data.events=window.ripleyFilterTrafficEvents(p._traffic_data.events,p,w.id);
   p._traffic_data_source=p._traffic_data.configured?'live':'unavailable';
  }catch(_e){p._traffic_data={configured:false,events:[]};p._traffic_data_source='unavailable';}
 }));
}
async function refreshPreviewCalendars(layout){
 const widgets=(layout.widgets||[]).filter(function(w){var p=w&&w.props||{};return w&&((w.widget_type||w.type)==='calendar')&&Array.isArray(p.template_elements)&&p.template_elements.length&&(p.ics_url||p.ical_url||p.ical||p.calendar_url);});
 await Promise.all(widgets.map(async function(w){try{var p=w.props;var url=p.ics_url||p.ical_url||p.ical||p.calendar_url;var r=await fetch('/api/ics-proxy?url='+encodeURIComponent(url),{cache:'no-store'});if(r.ok){p._calendar_events=ripleyParseCalendarText(await r.text(),ripleyCalendarRequiredEventCount(p),p.time_format === '24' ? '24' : '12');}}catch(_e){w.props._calendar_events=[];}}));
}
async function load(){
 // preview mode skips player.js init(), so load installed fonts here.
 try {
   const fontsResp=await fetch('/api/fonts/installed');
   if(fontsResp.ok && typeof injectInstalledFonts==='function') {
     injectInstalledFonts(await fontsResp.json());
     if(document.fonts && document.fonts.ready) await document.fonts.ready;
   }
 } catch(_fontError) { /* system fallback remains usable */ }
 if(!layoutId){ document.getElementById('title').textContent='No layout specified'; return; }
 const r=await fetch('/api/layouts/'+encodeURIComponent(layoutId)+'/render');
 if(!r.ok){ document.getElementById('title').textContent='Layout not found'; return; }
 const layout=await r.json();await ripleyHydrateTrafficTemplates(layout);await refreshPreviewWeather(layout);await refreshPreviewTraffic(layout);await refreshPreviewCalendars(layout);
 document.getElementById('title').textContent='Preview: '+layout.name+' • '+layout.id;
 if(window.RipleyPreviewRenderer){ renderShared(layout); return; }
 const stage=document.getElementById('stage'); ripleySetRenderOrientation(layout); stage.innerHTML='';
 const sorted=(layout.widgets||[]).slice().sort((a,b)=>(a.z||0)-(b.z||0));
 window.__previewLayout=layout;
 for(const w of sorted){ stage.appendChild(['button','textbox','dropdown','checkbox','checkbox-list','radio','keyboard'].includes(String(w.widget_type||'').toLowerCase()) ? renderInteractive(w) : renderWidget(w)); }
}
function isYouTubeUrl(u){return /(?:youtube\.com|youtu\.be)/i.test(String(u||''));}
function youtubeId(u){const s=String(u||'');let m=s.match(/[?&]v=([^&]+)/); if(m) return m[1]; m=s.match(/youtu\.be\/([^?&/]+)/); if(m) return m[1]; m=s.match(/youtube\.com\/(?:embed|shorts)\/([^?&/]+)/); return m?m[1]:'';}
function youtubeEmbedUrl(u,p){const id=youtubeId(u); const params=new URLSearchParams({autoplay:'1',mute:p.mute?'1':'0',loop:'1',playsinline:'1',rel:'0'}); if(id) params.set('playlist',id); return id ? ('https://www.youtube.com/embed/'+encodeURIComponent(id)+'?'+params.toString()) : String(u||'');}

function escCss(v){
  // Strip everything that isn't safe inside a CSS value: alnum,
  // spaces, commas, periods, percents, # for hex colors, parentheses,
  // hyphens, slashes, plus/minus. Keeps user-supplied colors like
  // '#e94560', 'rgba(0,0,0,0.5)', '120deg', etc. — and importantly,
  // removes ';', '"', '\'', '<', '>', and any URL-style 'javascript:'
  // sequences that would let a malicious prop break out of style="".
  return String(v||'').replace(/[^a-zA-Z0-9 #.,()%+\-/]/g,'');
}
function submitInteractive(w, payload, node){
 const layout=window.__previewLayout||{};
 fetch('/api/interactive/submit',{method:'POST',headers:{'Content-Type':'application/json','X-Ripley-Client-Id':'editor-preview'},body:JSON.stringify({client_id:'editor-preview',interactive_layout_id:layout.id||null,interactive_layout_name:layout.name||null,widget_id:w.id,widget_type:w.widget_type,widget_label:(w.props||{}).label||null,payload:payload,collected_at:new Date().toISOString()})}).then(function(r){return r.json().then(function(j){return {ok:r.ok,data:j};});}).then(function(result){
  var msg=document.getElementById('preview-status');
  if(msg){msg.textContent=result.ok?'Data collected (submission '+(result.data.id||'received')+')':'Submission failed: '+(result.data.error||'HTTP error');msg.className=result.ok?'ok':'error';}
  if(node) node.classList.toggle('submitted',result.ok);
 }).catch(function(e){var msg=document.getElementById('preview-status');if(msg){msg.textContent='Submission failed: '+e.message;msg.className='error';}});
}
function renderInteractive(w){
 const p=w.props||{}, t=(w.widget_type||'').toLowerCase(); const buttonLabel = p.action === 'page_switch' && /^go to page$/i.test(String(p.label || '')) ? 'Go to layout' : (p.label || 'Button');
 const rotation=Math.max(-180,Math.min(180,Number.isFinite(Number(p.rotation))?Number(p.rotation):0));
 const el=document.createElement('div'); el.className='widget interactive-preview-widget'; el.style.left=(Number(w.x)||0)+'%'; el.style.top=(Number(w.y)||0)+'%'; el.style.width=(Number(w.w)||0.1)+'%'; el.style.height=(Number(w.h)||0.1)+'%'; el.style.zIndex=Number(w.z)||1; el.style.transform='rotate('+rotation+'deg)'; el.style.transformOrigin='center center';
 const box=document.createElement('div'); box.className='interactive-preview-box'; el.appendChild(box);
 if(t==='textbox'){box.innerHTML='<label>'+esc(p.label||'Text')+'<input data-preview-value="value" value="'+esc(p.value||'')+'" placeholder="'+esc(p.placeholder||'')+'"></label>';box.querySelector('input').addEventListener('change',function(){submitInteractive(w,{value:this.value},el);});}
 else if(t==='dropdown'){box.innerHTML='<label>'+esc(p.label||'Choose')+'<select data-preview-value="selected"><option value="">— Select —</option>'+((p.options||[]).map(function(o){return '<option value="'+esc(o)+'">'+esc(o)+'</option>';}).join(''))+'</select></label>';box.querySelector('select').addEventListener('change',function(){submitInteractive(w,{selected:this.value},el);});}
 else if(t==='checkbox'){box.innerHTML='<label><input type="checkbox" data-preview-value="checked"> '+esc(p.label||'Checkbox')+'</label>';box.querySelector('input').addEventListener('change',function(){submitInteractive(w,{checked:this.checked},el);});}
 else if(t==='radio'){box.innerHTML='<fieldset><legend>'+esc(p.label||'Pick one')+'</legend>'+((p.options||[]).map(function(o){return '<label><input type="radio" name="preview-'+esc(w.id)+'" value="'+esc(o)+'"> '+esc(o)+'</label>';}).join(''))+'</fieldset>';box.querySelectorAll('input').forEach(function(i){i.addEventListener('change',function(){submitInteractive(w,{selected:this.value},el);});});}
 else if(t==='checkbox-list'){box.innerHTML='<fieldset><legend>'+esc(p.label||'Select items')+'</legend>'+((p.options||[]).map(function(o){return '<label><input type="checkbox" value="'+esc(o)+'"> '+esc(o)+'</label>';}).join(''))+'</fieldset>';box.querySelectorAll('input').forEach(function(i){i.addEventListener('change',function(){submitInteractive(w,{checkedValues:Array.from(box.querySelectorAll('input:checked')).map(function(x){return x.value;})},el);});});}
else {box.innerHTML='<button type="button">'+esc(buttonLabel)+'</button>';box.querySelector('button').addEventListener('click',async function(){
   if(p.action !== 'page_switch' || !(p.targetPageId||p.targetLayoutId)){submitInteractive(w,{action:'click'},el);return;}
   const targetId=p.targetPageId||p.targetLayoutId;
   const r=await fetch('/api/layouts/'+encodeURIComponent(targetId)+'/render');
   if(!r.ok){var msg=document.getElementById('preview-status');if(msg){msg.textContent='Target layout not found: '+targetId;msg.className='error';}return;}
   const target=await r.json(); window.__previewLayout=target; document.getElementById('title').textContent='Preview: '+target.name+' • '+target.id; document.getElementById('stage').innerHTML='';
   (target.widgets||[]).slice().sort((a,b)=>(a.z||0)-(b.z||0)).forEach(function(tw){document.getElementById('stage').appendChild(['button','textbox','dropdown','checkbox','checkbox-list','radio','keyboard'].includes(String(tw.widget_type||'').toLowerCase())?renderInteractive(tw):renderWidget(tw));});
   var msg=document.getElementById('preview-status');if(msg){msg.textContent='Switched to layout: '+target.name;msg.className='ok';}
 });}
 return el;
}
function renderWidget(w){
 const p=w.props||{}; const rawType=(w.widget_type||w.type||'').toLowerCase(); const t=rawType==='embed'?'webpage':rawType;
 const x=Number(w.x)||0,y=Number(w.y)||0,ww=Number(w.w)||0.1,hh=Number(w.h)||0.1,z=Number(w.z)||1;
 const el=document.createElement('div'); el.className='widget'; el.style.cssText=`left:${x}%;top:${y}%;width:${ww}%;height:${hh}%;z-index:${z};`;
 if(t==='text'){ const _fs=Number(p.font_size||p.fontSize||24)*2; const textContent=p.content ?? p.text ?? 'Text'; el.innerHTML=`<div class="text" style="color:${escCss(p.color||'#fff')};font-size:${_fs}px;text-align:${escCss(p.align||'center')};">${esc(textContent)}</div>`; }
 else if(t==='image'){ el.innerHTML=`<img src="${esc(p.url||'')}" style="width:100%;height:100%;object-fit:cover">`; }
 else if(t==='video'){
  const url=p.url||'';
  if(isYouTubeUrl(url)) { const fit=['cover','contain','fill','none','scale-down'].includes(p.fit)?p.fit:'cover'; el.innerHTML=`<iframe src="/youtube.html?url=${encodeURIComponent(url)}&fit=${encodeURIComponent(fit)}&v=${Date.now()}" style="width:100%;height:100%;border:0;background:#000;" allow="autoplay; encrypted-media; picture-in-picture; fullscreen"></iframe>`; }
  else el.innerHTML=`<video src="${esc(url)}" autoplay loop playsinline ${p.mute?'muted':''} style="width:100%;height:100%;object-fit:${['cover','contain','fill','none','scale-down'].includes(p.fit)?p.fit:'cover'}"></video>`;
 }
 else if(t==='music'){ el.innerHTML=`<audio src="${esc(p.url||'')}" autoplay loop ${p.mute?'muted':''}></audio>`; }
 else if(t==='clock'){ const c=document.createElement('div'); c.className='text'; c.style.fontSize=(Number(p.font_size||48)*2)+'px'; c.style.color=p.color||'#fff'; setInterval(()=>c.textContent=new Date().toLocaleTimeString([], {hour:'2-digit', minute:'2-digit'}),1000); el.appendChild(c); }
 else if(t==='datetime'||t==='date'){ const c=document.createElement('div'); c.className='text'; c.style.fontSize=(Number(p.font_size||36)*2)+'px'; c.style.color=p.color||'#fff'; setInterval(()=>c.textContent=new Date().toLocaleDateString([], {weekday:'long', month:'long', day:'numeric'}),60000); c.textContent=new Date().toLocaleDateString([], {weekday:'long', month:'long', day:'numeric'}); el.appendChild(c); }
 else if(t==='rectangle'){ el.innerHTML=`<div style="width:100%;height:100%;background:${escCss(p.color||'#e94560')};border-radius:${escCss(p.radius||0)}px"></div>`; }
 else if(t==='webpage'){ el.innerHTML=`<iframe src="${esc(p.url||'about:blank')}"></iframe>`; }
 else { el.innerHTML=`<div class="text" style="border:1px dashed #334155;color:#94a3b8">${esc(t||'widget')}</div>`; }
 return el;
}
function esc(s){return String(s??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));}
load();

