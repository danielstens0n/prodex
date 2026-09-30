import { invoke } from "@tauri-apps/api/core";
import "@awesome.me/webawesome/dist/styles/webawesome.css";
import "@awesome.me/webawesome/dist/components/button/button.js";
import type WaButton from "@awesome.me/webawesome/dist/components/button/button.js";
import "./styles.css";
import { icon, icons } from "./icons";

type Provider = "codex" | "claude" | "mock";
type Risk = "low" | "medium" | "high";
interface Settings {
  paused: boolean; max_concurrent: number; max_per_project: number;
  preferred_provider: Provider; task_timeout_secs: number; max_starts_per_day: number;
  planning_enabled: boolean; planner_provider: Provider; planner_cooldown_secs: number;
  max_plans_per_day: number; auto_approve_read_only: boolean; project_scope: string[] | null;
  planning_free_slots: number; max_proposal_risk: Risk;
}
interface Project { use_worktrees?:boolean; path: string; objective: string; objective_version: number; enabled: boolean }
interface Task { attempts?:unknown[]; started_at?:number|null; automatic?:boolean; id:string; status:string; review:string; proposal:{dependencies?:string[];brief?:{title:string;change:string;approach:string}|null;project:string;prompt:string;rationale:string;provider:Provider;mode:string;risk:Risk;completion_criteria:string}; summary:string; created_at:number; updated_at:number; worktree:string|null; session_id:string|null }
interface ObservedSession { id:string; cwd:string; project:string; last_seen:number; source:string }
interface Snapshot { protocol_version: number; settings: Settings; projects: Project[]; tasks: Task[]; planning_activity?:{project:string;message:string;next_check_at:number|null;daily_limit_reached?:boolean}[]; observation:{checked_at:number|null;available:boolean;detail:string;sessions:ObservedSession[]} }
interface Response<T> { ok: boolean; data: T; error: string | null }
const el = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
el("activity-projects").prepend(icon(icons.addProject, "start"));
el("settings-tab").prepend(icon(icons.settings));
el("toast-dismiss").replaceChildren(icon(icons.close));
const form = el<HTMLFormElement>("config-form");
let state: Snapshot | null = null;
let connected = false;
let busy = false;
let refreshing: Promise<void> | null = null;
let planningInitialized = false;
const enabled = () => state?.settings.planning_enabled ?? false;
const scope = () => state?.settings.project_scope ?? [];
let currentTab: "activity"|"config" = "activity";
const field = (name: string) => form.elements.namedItem(name) as HTMLSelectElement;

async function request<T>(data: Record<string, unknown>): Promise<T> {
  const response = await invoke<Response<T>>("request", {request:data});
  if (!response.ok) throw new Error(response.error ?? "Couldn’t save. Please try again.");
  return response.data;
}
const reportedFailures=new Set<string>();
let toastTimer:ReturnType<typeof setTimeout>|undefined;
function needsRetry(task:Task){return ["needs_retry","failed"].includes(task.status);}
function failureReason(task:Task) {
  const message=task.summary?.trim()||"The attempt couldn’t finish. Try again when you’re ready.";
  if(message.includes("not a git repository"))return "This project needs a Git repository with an initial commit before Prodex can create a coding worktree.";
  if(/unborn|valid HEAD|ambiguous argument 'HEAD'|unknown revision/i.test(message))return "Create an initial Git commit in this project before trying again.";
  return message.length>320 ? `${message.slice(0,317)}…` : message;
}
function notifyFailure(task:Task) {
  el("toast-message").textContent=`${task.proposal.brief?.title||sessionTitle(task.proposal.prompt)}: ${failureReason(task)}`;
  el("toast").hidden=false;clearTimeout(toastTimer);toastTimer=setTimeout(()=>{el("toast").hidden=true;},12000);
}
el("toast-dismiss").onclick=()=>{el("toast").hidden=true;clearTimeout(toastTimer);};
function showError(error: unknown) {
  const message = error instanceof Error ? error.message : String(error);
  el("error").textContent = message; el("error").title = message; el("error").hidden = false;
}
function controls() {
  document.querySelectorAll<WaButton>("wa-button").forEach(b => { b.disabled = busy || !connected; });
  document.querySelectorAll<HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement>("input,select,textarea").forEach(i => { i.disabled = busy || !connected; });
  document.querySelectorAll<HTMLButtonElement>('.project-remove,#disconnect-confirm').forEach(b=>b.disabled=busy||!connected);
  el<HTMLButtonElement>("disconnect-cancel").disabled=busy;

}
function render() {
  if (state) {
    const s = state.settings;
    if (![...field("max_concurrent").options].some(o => o.value === String(s.max_concurrent))) field("max_concurrent").add(new Option(String(s.max_concurrent),String(s.max_concurrent)));
    field("max_concurrent").value = String(s.max_concurrent);
    const threshold = field("planning_free_slots"); threshold.replaceChildren();
    for (let n=1; n<=s.max_concurrent; n++) threshold.add(new Option(`${n} free`,String(n)));
    threshold.value = String(Math.min(s.planning_free_slots ?? 2,s.max_concurrent));
    const interval=field("planner_cooldown_secs");
    if(![...interval.options].some(o=>o.value===String(s.planner_cooldown_secs)))interval.add(new Option(`${s.planner_cooldown_secs} seconds`,String(s.planner_cooldown_secs)));
    interval.value=String(s.planner_cooldown_secs);
    // Polling must not overwrite an unfinished numeric edit.
    if(document.activeElement!==el("daily-checks"))el<HTMLInputElement>("daily-checks").value=String(s.max_plans_per_day);
    field("max_proposal_risk").value = s.max_proposal_risk ?? "medium";
    el("helper").textContent = "Changes save automatically.";
  }
  renderActivity();
  renderWorkspaceSettings();
  controls();
}
async function refresh() {
  if (refreshing) return refreshing;
  refreshing = (async () => {
    try {
      let next = await request<Snapshot>({command:"status"});
      if (next.protocol_version !== 8) {
        showError("Restart the updated Prodex service to use these settings.");
        throw new Error("Service update required");
      }
      if (!planningInitialized) {
        // Replace the former global switch once when opening the desktop.
        // Never broaden a legacy/null scope or reconnect disabled projects.
        const projects = (next.settings.project_scope ?? []).filter(path => next.projects.some(p => p.path === path && p.enabled));
        if (projects.length && (!next.settings.planning_enabled || next.settings.paused)) {
          try {
            if (!next.settings.planning_enabled) await request({command:"set_activation",enabled:true,projects});
            else if (next.settings.paused) await request({command:"resume"});
            next = await request<Snapshot>({command:"status"});
          } catch (error) {
            showError(error);
            throw error;
          }
        }
        planningInitialized = true;
      }
      if (!connected) el("error").hidden=true;
      const failures=next.tasks.filter(needsRetry).sort((a,b)=>b.updated_at-a.updated_at);
      const unseen=failures.find(t=>!reportedFailures.has(`${t.id}:${t.updated_at}:${t.attempts?.length??0}`));
      failures.forEach(t=>reportedFailures.add(`${t.id}:${t.updated_at}:${t.attempts?.length??0}`));
      if(unseen)notifyFailure(unseen);
      state=next; connected=true; el("connection").textContent="Connected"; el("connection").dataset.connected="true";
    } catch {
      connected=false; el("connection").textContent="Disconnected"; el("connection").dataset.connected="false";
    }
    render();
  })();
  try { await refreshing; } finally { refreshing=null; }
}
async function mutate(data: Record<string, unknown>, after?:()=>void) {
  if (busy || !connected) { render(); return; }
  busy=true; controls();
  try {
    await request(data);
    el("error").hidden=true;
    if (refreshing) await refreshing;
    await refresh(); after?.();
  } catch(error) { showError(error); }
  finally { busy=false; render(); }
}
function panel(name: "activity"|"config") {
  for(const id of ["activity","config"]) el(`${id}-panel`).hidden = id!==name;
  el("error").hidden=true;
}
async function addProjects() {
  if (busy || !connected) return;
  busy=true; controls(); el("error").hidden=true;
  try {
    const paths=await invoke<string[]>("pick_projects");
    if (!paths.length) return;
    // Refresh after the native dialog: it can stay open while another client edits a goal.
    if (refreshing) await refreshing;
    await refresh();
    if (!connected) throw new Error("Reconnect to the Prodex service, then add your projects again.");
    for (const path of new Set(paths)) {
      const existing=state?.projects.find(project=>project.path===path);
      if (existing?.enabled && scope().includes(path)) continue;
      await request({command:"project",path,enabled:true,objective:existing?.objective ??
        "Find useful independent work toward this project's documented goals. Inspect the README, project instructions, and recent code changes. Propose only concrete tasks supported by repository evidence; abstain when the goal or independence of the work is unclear."});
    }
    planningInitialized = false;
  } catch(error) { showError(error); }
  finally {
    await refresh(); busy=false; render();
  }
}
function textNode(tag:string, text:string, className="") {
  const node=document.createElement(tag); node.textContent=text; node.className=className; return node;
}
function time(value:number) { return new Date(value*1000).toLocaleString([], {month:"short",day:"numeric",hour:"2-digit",minute:"2-digit"}); }
function duration(seconds:number) {
  const minutes=Math.floor(Math.max(0,seconds)/60);
  return minutes < 1 ? "<1m" : minutes < 60 ? `${minutes}m` : `${Math.floor(minutes/60)}h ${minutes%60}m`;
}
function sessionTitle(prompt:string) {
  const first=prompt.trim().split(/\n|(?<=[.!?])\s/)[0] || "Untitled session";
  return first.length > 120 ? `${first.slice(0,117).trimEnd()}…` : first;
}
function elapsed(task:Task) {
  if(task.started_at && ["running","stopping","succeeded","failed","interrupted"].includes(task.status)) {
    const end=["running","stopping"].includes(task.status) ? Date.now()/1000 : task.updated_at;
    return duration(end-task.started_at);
  }
  return "";
}

function statusIcon(status:string) {
  const labels:Record<string,string>={awaiting_approval:"Needs approval",queued:"Queued",starting:"Starting",running:"Running",stopping:"Stopping",succeeded:"Completed",failed:"Needs retry",needs_retry:"Needs retry",interrupted:"Stopped",rejected:"Rejected",recovery_required:"Needs attention"};
  const icon=textNode("span","","status-icon");icon.dataset.status=status;
  icon.title=labels[status]??"Unknown status";icon.setAttribute("role","img");icon.setAttribute("aria-label",icon.title);
  // Shape as well as color distinguishes complete, active, pending, and failed work.
  const svg=document.createElementNS("http://www.w3.org/2000/svg","svg");svg.setAttribute("viewBox","0 0 16 16");svg.setAttribute("aria-hidden","true");
  const path=document.createElementNS(svg.namespaceURI,"path");
  path.setAttribute("d",status==="succeeded" ? "M4 8l3 3 5-6" : ["failed","needs_retry","recovery_required"].includes(status) ? "M8 4v5m0 2v1" : ["rejected","interrupted"].includes(status) ? "M5 8h6" : ["running","starting","stopping"].includes(status) ? "M8 4v4l3 2" : "");
  svg.append(path);icon.append(svg);return icon;
}
function description(task:Task) {
  const body=textNode("div","","task-description");
  const brief=task.proposal.brief;
  const sections = brief ? [["Proposed change",brief.change],["Approach",brief.approach],["Verification",task.proposal.completion_criteria]] : [["Task",task.proposal.prompt],["Verification",task.proposal.completion_criteria]];
  for(const [label,value] of sections) {
    if(!value?.trim())continue;
    const section=textNode("section","","brief-section");
    section.append(textNode("h3",label),textNode("p",value));body.append(section);
  }
  return body;
}

function taskButton(label:string, command:string, id:string) {
  const button=document.createElement("wa-button") as WaButton;button.textContent=label;button.setAttribute("size","small");
  if(["approve","retry"].includes(command))button.setAttribute("variant","brand");
  button.dataset.action=command;button.onclick=()=>void mutate({command,id},()=>{
    if(command==="retry")el("toast").hidden=true;
    if(command==="reject") {
      const project=state?.tasks.find(t=>t.id===id)?.proposal.project;
      [...document.querySelectorAll<HTMLElement>("[data-history-project]")].find(b=>b.dataset.historyProject===project)?.focus();
    }
  }); return button;
}
type CopyKind = "prompt"|"resume";
let copied: {id:string;kind:CopyKind;until:number}|null=null;
const quote = (value:string) => "'" + value.replaceAll("'", "'\\''") + "'";
async function copyTask(id:string, kind:CopyKind) {
  if(busy || !connected)return;
  busy=true;copied=null;controls();
  try {
    const latest=await request<Snapshot>({command:"status"});
    const task=latest.tasks.find(t=>t.id===id);
    if(!task)throw new Error("This session is no longer available.");
    let text=task.proposal.prompt;
    if(kind!=="prompt") {
      if(!task.session_id || task.proposal.provider==="mock")throw new Error("This suggestion has no provider session yet.");
      text=task.session_id;
      if(kind==="resume") {
        const command=task.proposal.provider==="codex" ? "codex resume --include-non-interactive" : "claude --resume";
        text=`cd -- ${quote(task.worktree ?? task.proposal.project)} && ${command} ${quote(task.session_id)}`;
      }
    }
    await invoke("copy_text",{text});
    state=latest;el("error").hidden=true;copied={id,kind,until:Date.now()+2500};
  } catch(error) {showError(error);}
  finally {busy=false;render();}
}
function copyButton(label:string, kind:CopyKind, id:string) {
  const button=document.createElement("wa-button") as WaButton;
  const done=copied?.id===id && copied.kind===kind && copied.until>Date.now();
  button.textContent=done ? "Copied" : label;button.setAttribute("size","small");
  button.setAttribute("aria-label",label);button.dataset.action=`copy-${kind}`;
  button.onclick=()=>void copyTask(id,kind);return button;
}
interface Destination {id:string;label:string;kind:string}
let destinations:Destination[]=[{id:"terminal",label:"Terminal",kind:"terminal"}];
let preferredDestination="terminal";
try {preferredDestination=localStorage.getItem("prodex.openDestination")||"terminal";}catch{}
let destinationMenuFocused=false;
let openNotice:{id:string;text:string}|null=null;
function availableDestinations(task:Task) {
  return destinations.filter(d=>(d.kind!=="codex"||task.proposal.provider==="codex")&&(d.kind!=="claude"||task.proposal.provider==="claude"));
}
function destinationFor(task:Task) {
  const available=availableDestinations(task);
  return available.find(d=>d.id===preferredDestination)??available.find(d=>d.id==="terminal")??available[0];
}
async function loadDestinations() {
  try {destinations=await invoke<Destination[]>("list_destinations",{custom:preferredDestination.startsWith("app:")?preferredDestination:null});render();}
  catch { /* Existing Copy command remains available if discovery fails. */ }
}
async function chooseDestination(value:string) {
  try {
    if(value==="other") {
      const app=await invoke<Destination|null>("pick_destination");
      if(!app)return;
      destinations=destinations.filter(d=>d.id!==app.id);destinations.push(app);value=app.id;
    }
    preferredDestination=value;
    try {localStorage.setItem("prodex.openDestination",value);}catch{}
    openNotice=null;opened=null;
  } catch(error){showError(error);}
  finally {destinationMenuFocused=false;render();}
}
let opened: {id:string;action:string;until:number}|null=null;
async function openTask(id:string,action="terminal") {
  if(busy || !connected)return;
  busy=true;controls();opened=null;
  try {
    const task=state?.tasks.find(t=>t.id===id);
    const destination=task&&destinationFor(task);
    if(!destination)throw new Error("Choose an installed app, or use Copy command.");
    const notice=await invoke<string|null>("open_session",{id,action,destination:destination.id});
    openNotice=notice?{id,text:notice}:null;
    el("error").hidden=true;opened={id,action,until:Date.now()+2500};
  } catch(error) {showError(error);}
  finally {busy=false;render();}
}
function openButton(id:string, resolve=false) {
  const task=state!.tasks.find(t=>t.id===id)!;
  const destination=destinationFor(task);
  const label=destination?`${resolve?"Resolve in":"Open in"} ${destination.label}`:"Open in…";
  const group=document.createElement("span");group.className="open-destination";
  const button=document.createElement("wa-button") as WaButton;
  button.textContent=opened?.id===id && opened.action==="terminal" && opened.until>Date.now() ? "Opened" : label;
  button.setAttribute("size","small");button.setAttribute("variant","brand");button.setAttribute("aria-label",label);
  button.prepend(icon(icons.terminal,"start"));button.dataset.action="open-session";
  button.onclick=()=>void openTask(id);
  const select=document.createElement("select");select.className="destination-select";
  select.setAttribute("aria-label","Open session with");select.title="Choose an app · remembered for next time";
  for(const app of availableDestinations(task)) {
    const detail=app.kind==="editor"?" · folder + copied command":app.kind==="clipboard"?" · copied command":app.kind==="claude"?" · use /resume":"";
    select.add(new Option(app.label+detail,app.id));
  }
  select.add(new Option("Other app…","other"));select.value=destination?.id??"";
  select.onfocus=()=>{destinationMenuFocused=true;};
  select.onblur=()=>{destinationMenuFocused=false;};
  select.onchange=()=>void chooseDestination(select.value);
  group.append(button,select);return group;
}
function awaitsReview(task:Task) {return task.status==="succeeded" && ["awaiting_review","accepted"].includes(task.review) && (Boolean(task.worktree) || task.proposal.mode==="edit_in_place");}
const completionLabels={merge:"Merge locally",terminal:"Open in Terminal"};
type CompletionAction=keyof typeof completionLabels;
const savedActions=savedList("prodex.completionOrder").filter((a):a is CompletionAction=>Object.hasOwn(completionLabels,a));
let completionOrder:CompletionAction[]=[...new Set([...savedActions,...Object.keys(completionLabels) as CompletionAction[]])];
function completionButton(id:string, action:CompletionAction, primary:boolean) {
  if(action==="terminal"){const button=openButton(id);if(!primary)button.querySelector("wa-button")?.removeAttribute("variant");return button;}
  const button=document.createElement("wa-button") as WaButton;button.textContent=completionLabels[action];button.setAttribute("size","small");if(primary)button.setAttribute("variant","brand");button.dataset.action=`completion-${action}`;
  button.title="Continue this session in Terminal with focused instructions";
  if(action==="merge")button.title="Review and merge in the background. Done is verified automatically.";
  button.onclick=()=>action==="merge"?void mutate({command:"merge",id}):void openTask(id,action);return button;
}
function renderCompletionOrder() {
  const list=el("completion-order");list.replaceChildren();
  const move=(from:number,to:number)=>{if(to<0||to>=completionOrder.length)return;const next=[...completionOrder];next.splice(to,0,next.splice(from,1)[0]);completionOrder=next;try{localStorage.setItem("prodex.completionOrder",JSON.stringify(next));}catch{}renderCompletionOrder();renderActivity();};
  for(const [index,action] of completionOrder.entries()) {
    const row=textNode("div","","completion-order-row");row.setAttribute("role","listitem");row.draggable=true;row.dataset.completionAction=action;
    const grip=textNode("span","⋮⋮");grip.setAttribute("aria-hidden","true");row.append(grip,textNode("span",completionLabels[action]));
    row.ondragstart=e=>e.dataTransfer?.setData("application/x-prodex-action",action);
    row.ondragover=e=>{if(e.dataTransfer?.types.includes("application/x-prodex-action"))e.preventDefault();};
    row.ondrop=e=>{e.preventDefault();const from=completionOrder.indexOf(e.dataTransfer?.getData("application/x-prodex-action") as CompletionAction);if(from>=0)move(from,index);};
    for(const [label,offset] of [["up",-1],["down",1]] as const) {const b=textNode("button",offset<0?"↑":"↓") as HTMLButtonElement;b.type="button";b.setAttribute("aria-label",`Move ${completionLabels[action]} ${label}`);b.disabled=index+offset<0||index+offset>=completionOrder.length;b.onclick=()=>{move(index,index+offset);list.querySelector<HTMLElement>(`[data-completion-action="${action}"] button:not(:disabled)`)?.focus();};row.append(b);}
    list.append(row);
  }
}
let resolveChoice:((yes:boolean)=>void)|null=null;
function confirmChoice(title:string,description:string,confirm:string):Promise<boolean> {
  if(resolveChoice)return Promise.resolve(false);
  el("choice-title").textContent=title;el("choice-description").textContent=description;el("choice-confirm").textContent=confirm;
  el<HTMLDialogElement>("choice-dialog").showModal();el("choice-cancel").focus();return new Promise(resolve=>{resolveChoice=resolve;});
}
function finishChoice(yes:boolean){el<HTMLDialogElement>("choice-dialog").close();const resolve=resolveChoice;resolveChoice=null;resolve?.(yes);}
el("choice-cancel").onclick=()=>finishChoice(false);el("choice-confirm").onclick=()=>finishChoice(true);el("choice-dialog").addEventListener("cancel",e=>{e.preventDefault();finishChoice(false);});
function renderWorkspaceSettings() {
  const select=el<HTMLSelectElement>("workspace-project"), selected=select.value;
  const projects=state?.projects.filter(p=>p.enabled)??[];
  if(JSON.stringify([...select.options].map(o=>o.value))!==JSON.stringify(projects.map(p=>p.path)))select.replaceChildren(...projects.map(p=>new Option(p.path.split("/").at(-1)??p.path,p.path)));
  select.title=select.value;
  if([...select.options].some(o=>o.value===selected))select.value=selected;
  const project=state?.projects.find(p=>p.path===select.value), mode=el<HTMLSelectElement>("workspace-mode");mode.value=project?.use_worktrees===false ? "main" : "worktree";
  el("workspace-help").textContent=!project ? "Add a project in Activity to choose its workspace." : project.use_worktrees===false ? "Changes appear in your main folder immediately. Prodex runs one coding task at a time here; your own sessions can still edit the same files." : "Default: each coding task uses a separate Git worktree. Review and merge its changes when finished.";
}
el("workspace-project").onchange=e=>{e.stopPropagation();renderWorkspaceSettings();};
el("workspace-mode").onchange=async e=>{
  e.stopPropagation();const project=el<HTMLSelectElement>("workspace-project").value,enabled=el<HTMLSelectElement>("workspace-mode").value==="worktree";renderWorkspaceSettings();
  if(!project)return;
  if(!enabled && !await confirmChoice("Edit the main folder directly?","New tasks will change the same files you work on. Prodex will run one coding task at a time in this project. Existing worktree results stay separate.","Use main folder"))return;
  void mutate({command:"set_worktrees",project,enabled});
};
renderCompletionOrder();
function savedList(key:string):string[] {
  try {const value=JSON.parse(localStorage.getItem(key)??"[]");return Array.isArray(value)?value.filter(v=>typeof v==="string"):[];}catch{return [];}
}
let projectOrder=savedList("prodex.projectOrder");
const collapsedProjects=new Set(savedList("prodex.collapsedProjects"));
let draggedProject:string|null=null;
function saveProjectLayout() {try {localStorage.setItem("prodex.projectOrder",JSON.stringify(projectOrder));localStorage.setItem("prodex.collapsedProjects",JSON.stringify([...collapsedProjects]));}catch{/* Layout remains usable without persistent storage. */}}
function moveProject(path:string,target:string) {
  const paths=visibleProjects();const from=paths.indexOf(path),to=paths.indexOf(target);
  if(from<0||to<0||from===to)return;
  paths.splice(from,1);paths.splice(to,0,path);projectOrder=paths;saveProjectLayout();renderActivity();
}
function visibleProjects() {
  return (state?.projects.filter(p=>p.enabled && scope().includes(p.path)).map(p=>p.path)??[]).sort((a,b)=>{
    const rank=(p:string)=>projectOrder.indexOf(p)<0?Number.MAX_SAFE_INTEGER:projectOrder.indexOf(p);
    return rank(a)-rank(b);
  });
}
const disconnectDialog=el<HTMLDialogElement>("disconnect-dialog");
let disconnectPath:string|null=null;
function confirmDisconnect(path:string) {
  if(busy||!connected)return;
  disconnectPath=path;el("disconnect-name").textContent=path;el("disconnect-error").hidden=true;
  disconnectDialog.showModal();el("disconnect-cancel").focus();
}
el("disconnect-cancel").onclick=()=>{disconnectDialog.close();disconnectPath=null;};
disconnectDialog.addEventListener("cancel",event=>{if(busy)event.preventDefault();else disconnectPath=null;});
el("disconnect-confirm").onclick=async()=>{
  if(!disconnectPath||busy||!connected)return;
  busy=true;controls();
  try {await request({command:"unconnect_project",project:disconnectPath});disconnectDialog.close();disconnectPath=null;await refresh();el("activity-projects").focus();}
  catch(error){el("disconnect-error").textContent=error instanceof Error?error.message:String(error);el("disconnect-error").hidden=false;}
  finally {busy=false;render();}
};
function projectHeader(path:string,index:number,live:boolean) {
  const name=path.split("/").filter(Boolean).at(-1)??path;
  const header=textNode("div","","project-header");
  const handle=textNode("button","","project-drag") as HTMLButtonElement;handle.append(icon(icons.drag));
  handle.type="button";handle.draggable=true;handle.title="Drag to reorder, or use the up and down arrow keys";handle.setAttribute("aria-label",`Reorder ${name}`);handle.dataset.projectAction="reorder";handle.dataset.path=path;
  handle.ondragstart=e=>{draggedProject=path;e.dataTransfer?.setData("text/plain",path);if(e.dataTransfer)e.dataTransfer.effectAllowed="move";};
  handle.ondragend=()=>{draggedProject=null;renderActivity();};
  handle.onkeydown=e=>{if(["ArrowUp","ArrowDown"].includes(e.key)){e.preventDefault();const paths=visibleProjects();const target=paths[index+(e.key==="ArrowUp"?-1:1)];if(target)moveProject(path,target);}};
  const heading=textNode("h2","");const toggle=textNode("button","","project-toggle") as HTMLButtonElement;
  toggle.type="button";toggle.dataset.projectAction="collapse";toggle.dataset.path=path;toggle.title=path;
  toggle.setAttribute("aria-expanded",String(!collapsedProjects.has(path)));toggle.setAttribute("aria-controls",`project-content-${index}`);
  const arrow=textNode("span","","project-arrow");arrow.setAttribute("aria-hidden","true");
  arrow.append(icon(icons.chevron));
  toggle.append(arrow,textNode("span",name));toggle.onclick=()=>{if(collapsedProjects.has(path))collapsedProjects.delete(path);else collapsedProjects.add(path);saveProjectLayout();renderActivity();};heading.append(toggle);
  const presence=textNode("span","","session-indicator");
  presence.dataset.live=String(live);presence.title=live ? "Live code session" : "No session";
  presence.setAttribute("role","img");presence.setAttribute("aria-label",presence.title);heading.append(presence);
  const remove=textNode("button","","project-remove") as HTMLButtonElement;remove.append(icon(icons.close));remove.type="button";remove.title="Unconnect project";remove.setAttribute("aria-label",`Unconnect ${name}`);remove.dataset.projectAction="unconnect";remove.dataset.path=path;remove.disabled=busy||!connected;remove.onclick=()=>confirmDisconnect(path);
  header.append(handle,heading,remove);return header;
}
const revealedHistory = new Set<string>();
function renderActivity() {
  if(destinationMenuFocused)return;
  if(draggedProject)return;
  const list=el("activity-list");
  const expanded=new Set([...list.querySelectorAll<HTMLDetailsElement>("details[open]")].map(d=>d.dataset.id));
  const focused=document.activeElement as HTMLElement|null;
  const focusTask=focused?.closest<HTMLDetailsElement>("details[data-id]")?.dataset.id;
  const focusAction=focused?.closest<HTMLElement>("[data-action]")?.dataset.action;
  const focusedProject=focused?.closest<HTMLElement>("[data-project-action]");
  const focusProjectPath=focusedProject?.dataset.path,focusProjectAction=focusedProject?.dataset.projectAction;
  const focusHistory=focused?.closest<HTMLElement>("[data-history-project]")?.dataset.historyProject;
  const scrollTop=list.scrollTop;
  list.replaceChildren();
  if(!state) { list.append(textNode("p","Connect to the Prodex service to view activity.","empty")); return; }
  const observation=state.observation;
  el("observation-status").textContent=!connected ? "Offline · showing last known activity" : observation.available ? "Codex terminal sessions trigger allowed projects." : `Observation unavailable. ${observation.detail}`;
  el("observation-status").title=observation.checked_at ? `Last checked ${time(observation.checked_at)}. ${observation.detail}` : observation.detail;
  const paths=visibleProjects();
  if(!paths.length)list.append(textNode("p","Allow a project folder to get started. Work begins when an independent Codex session is observed there.","empty"));
  for(const path of paths) {
    const group=textNode("section","","project-activity"); group.dataset.project=path;
    const sessions=observation.sessions.filter(session=>session.project===path);
    const live=connected && observation.available && sessions.length>0;
    const index=paths.indexOf(path);group.append(projectHeader(path,index,live));
    const content=textNode("div","","project-content");content.id=`project-content-${index}`;content.hidden=collapsedProjects.has(path);group.append(content);
    group.ondragover=e=>{if(draggedProject){e.preventDefault();if(e.dataTransfer)e.dataTransfer.dropEffect="move";}};
    group.ondrop=e=>{e.preventDefault();const source=draggedProject;draggedProject=null;if(source)moveProject(source,path);};
    const allowed=scope().includes(path);
    const planning=state.planning_activity?.find(p=>p.project===path);
    let hasPlanningStatus=false;
    if(connected && enabled() && !state.settings.paused && planning && planning.message!=="Task needs attention") {
      hasPlanningStatus=true;
      const label=planning.next_check_at ? `${planning.daily_limit_reached ? "Daily check limit · resets" : "Next check"} in ${duration(planning.next_check_at-Date.now()/1000)}` : planning.message;
      const progress=textNode("p",label,"planning-status");progress.title=planning.message;
      if(planning.message.startsWith("Could not"))progress.textContent=`${planning.message}. ${label}`;
      const row=textNode("div","","planning-row");row.append(progress);
      if(planning.next_check_at && !planning.daily_limit_reached && allowed && live) {
        const check=document.createElement("wa-button") as WaButton;check.textContent="Check now";check.setAttribute("size","small");check.setAttribute("appearance","plain");check.dataset.projectAction="check";check.dataset.path=path;check.onclick=()=>void mutate({command:"check_now",project:path});row.append(check);
      }
      content.append(row);
    }
    const projectTasks=state.tasks.filter(t=>t.proposal.project===path && t.proposal.mode!=="merge").sort((a,b)=>Number(b.proposal.mode==="initialize_repository")-Number(a.proposal.mode==="initialize_repository")||b.updated_at-a.updated_at);
    const isArchived=(t:Task)=>t.status==="rejected" || (t.proposal.mode==="initialize_repository" && t.status==="interrupted") || (t.status==="succeeded" && !awaitsReview(t));
    const archived=projectTasks.filter(isArchived);
    const historyLabel=archived.some(t=>t.status==="succeeded") ? archived.some(t=>t.status==="rejected") ? "completed and rejected" : "completed" : archived.some(t=>t.status==="rejected") ? "rejected" : "past tasks";
    // Store order preserves the planner's ranked array across refresh/restart.
    const pending=state.tasks.filter(t=>t.proposal.project===path && t.status==="awaiting_approval" && t.proposal.mode!=="merge");
    pending.sort((a,b)=>Number(b.proposal.mode==="initialize_repository")-Number(a.proposal.mode==="initialize_repository"));
    const topIdeas=pending.slice(0,3);
    const tasks=projectTasks.filter(t=>!isArchived(t) && t.status!=="awaiting_approval");
    tasks.push(...topIdeas);
    if(pending.length>3)content.append(textNode("p",`Top 3 suggestions · ${pending.length-3} more in reserve`,"helper"));
    const history=textNode("div","","task-history");history.id=`history-${paths.indexOf(path)}`;
    if(!tasks.length && !hasPlanningStatus)content.append(textNode("p","No current ideas.","helper"));
    if(revealedHistory.has(path))tasks.push(...archived);
    for(const task of tasks) {
      const integration=state.tasks.filter(t=>t.proposal.mode==="merge"&&t.proposal.dependencies?.includes(task.id)).reverse().sort((a,b)=>b.created_at-a.created_at)[0];
      const merging=integration&&["queued","starting","running","stopping","recovery_required"].includes(integration.status);
      const card=document.createElement("details");card.className="task";card.dataset.id=task.id;card.open=expanded.has(task.id);
      const summary=textNode("summary", "");
      const title=textNode("span",task.proposal.brief?.title || sessionTitle(task.proposal.prompt),"task-title");title.title=task.proposal.prompt;
      const age=textNode("span",elapsed(merging?integration:task),"task-meta");age.title=awaitsReview(task) ? "Ready to review" : task.status.replaceAll("_"," ");
      const chevron=textNode("span","","task-chevron");chevron.setAttribute("aria-hidden","true");
      chevron.append(icon(icons.chevron));
      summary.append(statusIcon(merging?integration.status:task.status),title,age,chevron);card.append(summary);
      const body=textNode("div","","task-body");
      if(task.automatic && task.status==="queued" && !sessions.length)body.append(textNode("p","Waiting for Codex before launch."));
      const setupBlocked=task.proposal.dependencies?.some(id=>state?.tasks.some(t=>t.id===id && t.proposal.mode==="initialize_repository" && t.status!=="succeeded"));
      if(setupBlocked)body.append(textNode("p","Complete Git setup first. This coding task will use a worktree.","helper"));
      if(task.proposal.mode==="initialize_repository")body.append(textNode("p","Runs directly in this project folder to set up Git.","helper"));
      const direct=["awaiting_approval","queued","needs_retry","failed"].includes(task.status) && ["edit","edit_in_place"].includes(task.proposal.mode) ? state.projects.find(p=>p.path===path)?.use_worktrees===false : task.proposal.mode==="edit_in_place";
      if(direct)body.append(textNode("p","Edits the main folder directly. Changes appear immediately.","helper"));
      if(awaitsReview(task) && !integration)body.append(textNode("p",task.worktree ? "Ready to review · changes are still in the worktree." : "Ready to review · changes are already in your project.","helper"));
      if(integration && integration.status!=="succeeded")body.append(textNode("p",merging?(integration.status==="recovery_required"?"Merge needs recovery · verify the previous worker stopped before retrying.":integration.status==="queued"?"Merge queued · waiting for capacity.":integration.status==="stopping"?"Stopping merge…":"Merging locally…"):needsRetry(integration)?`Merge needs attention: ${failureReason(integration)}`:"Merge stopped. You can try again.","helper"));
      const actions=textNode("div","","task-actions");
      if(needsRetry(task)) {
        body.append(textNode("p",failureReason(task),"task-retry-reason"));
        if(!setupBlocked)actions.append(taskButton("Try again","retry",task.id));
        actions.append(taskButton("Reject","reject",task.id));
      }
      if(task.status==="awaiting_approval"){if(!setupBlocked)actions.append(taskButton("Approve","approve",task.id));actions.append(taskButton("Reject","reject",task.id));}
      if(["queued","starting","running"].includes(task.status))actions.append(taskButton("Stop task","stop",task.id));
      if(merging && integration.status!=="recovery_required")actions.append(taskButton("Stop merge","stop",integration.id));
      const mergeBlocked=Boolean(integration && !merging && integration.status!=="succeeded" && awaitsReview(task));
      if(task.session_id && task.proposal.provider!=="mock") {
        if(task.status==="succeeded") {
          let first=true;
          for(const action of completionOrder) {
            if(action!=="terminal" && !task.worktree)continue;
            if(action!=="terminal" && merging)continue;
            const button=action==="terminal" && mergeBlocked ? openButton(integration?.session_id?integration.id:task.id,true) : completionButton(task.id,action,first);
            if(action==="merge" && mergeBlocked)button.textContent="Retry merge";
            first=false;actions.append(button);
          }
        } else actions.append(openButton(task.id));
        actions.append(copyButton("Copy command","resume",mergeBlocked && integration?.session_id?integration.id:task.id));
      } else {
        actions.append(copyButton("Copy prompt","prompt",task.id));
        if(task.status==="awaiting_approval")body.append(textNode("p","A session is created when this suggestion runs.","helper"));
      }
      if(openNotice && (openNotice.id===task.id || openNotice.id===integration?.id))body.append(textNode("p",openNotice.text,"helper"));
      if(mergeBlocked)body.append(textNode("p","Resolve in your preferred app to continue the merge session, or select an IDE to open the worktree. After resolving or merging manually, Retry merge checks Git first; it only starts an agent if integration is still needed.","helper"));
      if(awaitsReview(task) && !merging) {
        const dismiss=document.createElement("wa-button") as WaButton;dismiss.setAttribute("size","small");dismiss.textContent="Dismiss result";dismiss.dataset.action="dismiss-result";
        dismiss.onclick=async()=>{if(await confirmChoice("Dismiss this result?","This hides the task in rejected history. Your changes, worktree and sessions are kept. Nothing is merged, undone or deleted, and dependent tasks remain blocked.","Dismiss result"))void mutate({command:"reject",id:task.id});};actions.append(dismiss);
      }
      if(awaitsReview(task) && !task.worktree) {
        const done=document.createElement("wa-button") as WaButton;done.setAttribute("size","small");done.textContent=task.worktree ? "Mark as integrated…" : "Mark reviewed";done.dataset.action="acknowledge-result";
        done.onclick=async()=>{const yes=await confirmChoice(task.worktree ? "Have you integrated these changes?" : "Have you reviewed these changes?",task.worktree ? "Confirm only after the changes are merged or applied to your project. Creating a PR is not enough. This records your confirmation and allows dependent tasks to run; it does not merge any files." : "The changes are already in your main folder. This records your review and allows dependent tasks to run.","Confirm");if(yes)void mutate({command:task.worktree ? "confirm_integrated" : "mark_reviewed",id:task.id});};actions.append(done);
      }
      body.append(actions,description(task));card.append(body);
      (isArchived(task) ? history : content).append(card);
    }
    if(archived.length) {
      const toggle=textNode("button",`${revealedHistory.has(path) ? "Hide" : "Show"} ${historyLabel} (${archived.length})`,"history-toggle") as HTMLButtonElement;
      toggle.type="button";toggle.dataset.historyProject=path;
      toggle.setAttribute("aria-expanded",String(revealedHistory.has(path)));toggle.setAttribute("aria-controls",history.id);
      history.hidden=!revealedHistory.has(path);
      toggle.onclick=()=>{if(revealedHistory.has(path))revealedHistory.delete(path);else revealedHistory.add(path);renderActivity();};
      content.append(toggle,history);
    }
    list.append(group);
  }
  if(focusTask) {
    const card=[...list.querySelectorAll<HTMLDetailsElement>("details[data-id]")].find(d=>d.dataset.id===focusTask);
    const target=focusAction ? [...(card?.querySelectorAll<HTMLElement>("[data-action]")??[])].find(b=>b.dataset.action===focusAction) : card?.querySelector<HTMLElement>("summary");
    const project=state.tasks.find(t=>t.id===focusTask)?.proposal.project;
    const fallback=[...list.querySelectorAll<HTMLElement>("[data-history-project]")].find(b=>b.dataset.historyProject===project);
    (target??card?.querySelector<HTMLElement>("summary")??fallback)?.focus({preventScroll:true});
  }
  if(focusHistory)[...list.querySelectorAll<HTMLElement>("[data-history-project]")].find(b=>b.dataset.historyProject===focusHistory)?.focus({preventScroll:true});
  if(focusProjectPath)[...list.querySelectorAll<HTMLElement>("[data-project-action]")].find(b=>b.dataset.path===focusProjectPath&&b.dataset.projectAction===focusProjectAction)?.focus({preventScroll:true});
  list.scrollTop=scrollTop;
}
function selectTab(tab:"activity"|"config") {
  currentTab=tab;panel(tab);
  for(const [id,value] of [["activity-tab","activity"],["settings-tab","config"]]) {
    el(id).setAttribute("aria-selected",String(value===tab));el(id).tabIndex=value===tab?0:-1;
  }
}
el("activity-tab").onclick=()=>selectTab("activity");
el("settings-tab").onclick=()=>selectTab("config");
for(const id of ["activity-tab","settings-tab"])el(id).onkeydown=event=>{
  if(["ArrowLeft","ArrowRight","Home","End"].includes(event.key)) {event.preventDefault();const tab=event.key==="Home"?"activity":event.key==="End"?"config":currentTab==="activity"?"config":"activity";selectTab(tab);el(tab==="activity"?"activity-tab":"settings-tab").focus();}
};
for(const name of ["general","workspaces","completion"])el(`${name}-page`).onclick=()=>{
  for(const page of ["general","workspaces","completion"]){el(`${page}-settings`).hidden=page!==name;el(`${page}-page`).setAttribute("aria-pressed",String(page===name));}
};
el("activity-projects").onclick=()=>void addProjects();
form.addEventListener("change",()=>{
  if(!state||busy)return;
  const daily=el<HTMLInputElement>("daily-checks");
  const dailyLimit=Number(daily.value);
  if(!daily.checkValidity()||!Number.isSafeInteger(dailyLimit)||dailyLimit<1){daily.setAttribute("aria-invalid","true");showError("Daily check limit must be a positive whole number.");return;}
  daily.removeAttribute("aria-invalid");
  const count=Number(field("max_concurrent").value);
  void mutate({command:"configure",settings:{...state.settings,
    max_concurrent:count,max_per_project:count,
    planning_free_slots:Math.min(Number(field("planning_free_slots").value),count),
    planner_cooldown_secs:Number(field("planner_cooldown_secs").value),max_plans_per_day:dailyLimit,
    auto_approve_read_only:false,max_proposal_risk:field("max_proposal_risk").value}});
});
form.onsubmit=e=>e.preventDefault();
controls();void loadDestinations();void refresh();setInterval(()=>{if(!busy)void refresh();},2000);
