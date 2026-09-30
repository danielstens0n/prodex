import { test, expect, type Page } from '@playwright/test';
async function setup(page: Page, initialSettings: Record<string, unknown> = {}) {
  await page.addInitScript((initialSettings) => {
    const state = {
      protocol_version: 8,
      settings: {paused: false, planning_enabled: true, preferred_provider: 'codex', planner_provider: 'claude', max_concurrent: 3, max_per_project: 3,
        task_timeout_secs: 600, max_starts_per_day: 20, planner_cooldown_secs: 60, max_plans_per_day: 1440, auto_approve_read_only: false,
        project_scope: Array.from({length:9}, (_,i)=>`/projects/app-${i+1}`) as string[] | null, planning_free_slots: 2, max_proposal_risk: 'medium'},
      projects: Array.from({length:9}, (_,i)=>({path:`/projects/app-${i+1}`,objective:'Build the application',objective_version:1,enabled:true})),
      tasks: [] as any[],
      observation: {checked_at:1700000000,available:true,detail:"Live sessions checked",sessions:[] as any[]},
    };
    Object.assign(state.settings, initialSettings);
    Object.assign(window, {__testState:state, __testRequests:[], __failNext:false, __pickedProjects:[], __pickerCalls:0, __pickerError:false, __clipboardText:null, __copyFailure:false, __openedSession:null, __openFailure:false,
      __TAURI_INTERNALS__: {invoke: async (_command: string, args: {request:Record<string,any>}) => {
        if (_command==='list_destinations') return (window as any).__destinations??[{id:'terminal',label:'Terminal',kind:'terminal'},{id:'ghostty',label:'Ghostty',kind:'terminal'},{id:'zed',label:'Zed',kind:'editor'},{id:'codex',label:'Codex',kind:'codex'},{id:'claude',label:'Claude Code',kind:'claude'}];
        if (_command==='pick_destination') return (window as any).__pickedDestination??null;
        if (_command==='open_session') {
          (window as any).__openedDestination=(args as any).destination;
          if ((window as any).__openFailure) throw new Error('Could not open Terminal. Use Copy command instead.');
          (window as any).__openedSession=(args as any).id;(window as any).__openedAction=(args as any).action;return (window as any).__openNotice??null;
        }
        if (_command==='copy_text') {
          if ((window as any).__copyFailure) throw new Error('Could not copy to clipboard');
          (window as any).__clipboardText=(args as any).text;return;
        }
        if (_command==='pick_projects') {
          (window as any).__pickerCalls++;
          if ((window as any).__pickerError) throw new Error('Folder picker unavailable');
          return (window as any).__pickedProjects;
        }
        const req=args.request; (window as any).__testRequests.push(req);
        if(req.command==='status') return {ok:true,data:structuredClone(state),error:null};
        if((window as any).__failNext) { (window as any).__failNext=false; return {ok:false,error:'Could not save settings',data:null}; }
        if(req.command==='set_activation') {
          state.settings.paused=!req.enabled || (state.settings.planning_enabled && state.settings.paused);state.settings.planning_enabled=req.enabled;state.settings.project_scope=req.projects;
          state.projects.forEach(p=>p.enabled=req.projects===null||req.projects.includes(p.path));
        }
        if(req.command==='unconnect_project'){state.projects.find(p=>p.path===req.project)!.enabled=false;state.settings.project_scope=state.settings.project_scope!.filter(p=>p!==req.project);}
        if(req.command==='check_now'){(state as any).planning_activity=[{project:req.project,message:'Looking for work…',next_check_at:null}];}
        if(req.command==='set_worktrees')(state.projects.find(p=>p.path===req.project)! as any).use_worktrees=req.enabled;
        if(req.command==='merge'){const source=state.tasks.find(t=>t.id===req.id);state.tasks.push({...structuredClone(source),id:'merge-job',status:'running',review:'not_required',created_at:Date.now()/1000,proposal:{...source.proposal,mode:'merge',dependencies:[source.id]}});}
        if(['mark_reviewed','confirm_integrated'].includes(req.command))state.tasks.find(t=>t.id===req.id).review='integrated';
        if(req.command==='configure')Object.assign(state.settings,req.settings);
        if(req.command==='pause')state.settings.paused=true;
        if(req.command==='resume')state.settings.paused=false;
        if(req.command==='project'){const existing=state.projects.find(p=>p.path===req.path);if(existing){existing.enabled=true;existing.objective=req.objective;}else state.projects.push({path:req.path,objective:req.objective,objective_version:1,enabled:true});if(!state.settings.project_scope?.includes(req.path))state.settings.project_scope?.push(req.path);}
        if(['approve','retry','reject','stop'].includes(req.command)){const t=state.tasks.find(t=>t.id===req.id);if(t)t.status=['approve','retry'].includes(req.command)?'queued':req.command==='reject'?'rejected':'interrupted';}
        return {ok:true,data:{accepted:true},error:null};
      }}
    });
  }, initialSettings);
  await page.goto('/');
  await expect(page.locator('#connection')).toHaveText('Connected');
}
async function noScroll(page: Page) {
  expect(await page.evaluate(()=>({x:document.documentElement.scrollWidth>innerWidth,y:document.documentElement.scrollHeight>innerHeight}))).toEqual({x:false,y:false});

}

test('settings remain compact without an agent selector',async({page})=>{
  await setup(page);await page.locator('#settings-tab').click();await noScroll(page);
  await expect(page.getByRole('heading',{name:'Prodex',exact:true})).toBeVisible();
  await expect(page.locator('#activity-panel')).toBeHidden();
  await expect(page.locator('#threshold')).toHaveValue('2');
  await expect(page.locator('#agent')).toHaveCount(0);
  await expect(page.getByRole('heading',{name:'Workload',exact:true})).toBeVisible();
  await expect(page.locator('.settings-sidebar,.settings-nav')).toHaveCount(0);
  await page.screenshot({path:'/tmp/prodex-preferences.png'});
});

test('global controls are absent and connected projects need no activation action',async({page})=>{
  await setup(page);
  await expect(page.locator('#power,#pause,#stop,footer.controls')).toHaveCount(0);
  expect(await page.evaluate(()=>(window as any).__testRequests.filter((r:any)=>r.command!=='status'))).toEqual([]);
});

for (const planningEnabled of [false,true]) {
  test(`opening a legacy ${planningEnabled?'paused':'off'} setup enables discovery without approving tasks`,async({page})=>{
    await setup(page,{paused:true,planning_enabled:planningEnabled});
    expect(await page.evaluate(()=>(window as any).__testState.settings.planning_enabled)).toBe(true);
    expect(await page.evaluate(()=>(window as any).__testState.settings.paused)).toBe(false);
    expect(await page.evaluate(()=>(window as any).__testRequests.filter((r:any)=>r.command!=='status').map((r:any)=>r.command))).toEqual([planningEnabled?'resume':'set_activation']);
  });
}

test('opening legacy null scope never enables discovery for all projects',async({page})=>{
  await setup(page,{paused:true,planning_enabled:false,project_scope:null});
  expect(await page.evaluate(()=>(window as any).__testRequests.filter((r:any)=>r.command!=='status'))).toEqual([]);
  await expect(page.locator('.project-activity')).toHaveCount(0);
});

test('settings show all sections on one page at the minimum window size',async({page})=>{
  await setup(page);await page.locator('#settings-tab').click();
  await page.setViewportSize({width:540,height:620});await noScroll(page);
  for(const name of ['workload','planning','approvals'])await expect(page.locator(`#${name}-settings`)).toBeVisible();
  await expect(page.locator('#autonomy')).toHaveCount(0);
  await expect(page.getByText('Every task still needs your approval.')).toBeVisible();
  const sizes=await page.locator('.settings-content h2,.settings-content label,.settings-content p').evaluateAll(nodes=>nodes.map(n=>getComputedStyle(n).fontSize));
  expect(new Set(sizes).size).toBe(1);
  await page.screenshot({path:'/tmp/prodex-settings-compact.png'});
});

test('configuration saves threshold, autonomy and risk, and clamps to capacity',async({page})=>{
  await setup(page);await page.locator('#settings-tab').click();
  await page.locator('#threshold').selectOption('3');
  await expect.poll(()=>page.evaluate(()=>(window as any).__testState.settings.planning_free_slots)).toBe(3);
  await expect(page.locator('#autonomy')).toHaveCount(0);
  await expect.poll(()=>page.evaluate(()=>(window as any).__testState.settings.auto_approve_read_only)).toBe(false);
  
  await page.locator('#risk').selectOption('low');
  await expect.poll(()=>page.evaluate(()=>(window as any).__testState.settings.max_proposal_risk)).toBe('low');
  
  await page.locator('#threads').selectOption('1');
  await expect.poll(()=>page.evaluate(()=>(window as any).__testState.settings.planning_free_slots)).toBe(1);
  await expect(page.locator('#threshold')).toHaveValue('1');
  expect(await page.evaluate(() => {const s=(window as any).__testState.settings;return [s.preferred_provider,s.planner_provider];})).toEqual(['codex','claude']);
});

test('Add projects sits below the text and opens the native picker directly',async({page})=>{
  await setup(page);
  const button=page.getByText('Add projects',{exact:true});
  const text=await page.locator('#observation-status').boundingBox();
  const bounds=await button.boundingBox();
  expect(bounds!.y).toBeGreaterThanOrEqual(text!.y+text!.height);
  expect(Math.abs(bounds!.x-text!.x)).toBeLessThan(2);
  await expect(page.locator('#projects-panel,#add-panel,#project-form,#scope-menu')).toHaveCount(0);
  await page.evaluate(()=>(window as any).__pickedProjects=['/projects/new one','/projects/new-two','/projects/new one','/projects/app-1']);
  await button.click();
  await expect.poll(()=>page.evaluate(()=>(window as any).__testState.projects.length)).toBe(11);
  expect(await page.evaluate(()=>(window as any).__pickerCalls)).toBe(1);
  const added=await page.evaluate(()=>(window as any).__testRequests.filter((r:any)=>r.command==='project'));
  expect(added.map((r:any)=>r.path)).toEqual(['/projects/new one','/projects/new-two']);
  expect(added.every((r:any)=>r.enabled && r.objective.includes('documented goals'))).toBe(true);
  await expect(page.locator('#activity-panel')).toBeVisible();
  await noScroll(page);
  await page.screenshot({path:'/tmp/prodex-picker-ui.png'});
});

test('failed settings save rolls back; disconnected controls cannot launch work',async({page})=>{
  await setup(page);await page.locator('#settings-tab').click();
  await page.evaluate(()=>(window as any).__failNext=true);
  await page.locator('#threads').selectOption('4');await expect(page.locator('#error')).toBeVisible();
  await expect(page.locator('#threads')).toHaveValue('3');
  await page.evaluate(()=>(window as any).__TAURI_INTERNALS__.invoke=()=>Promise.reject(new Error('offline')));
  await expect(page.locator('#connection')).toHaveText('Disconnected');
  await expect(page.locator('#threads')).toBeDisabled();await noScroll(page);
});

test('picker cancellation and errors never add projects',async({page})=>{
  await setup(page);
  await page.locator('#activity-projects').click();
  await expect.poll(()=>page.evaluate(()=>(window as any).__pickerCalls)).toBe(1);
  await expect(page.locator('#activity-projects')).toHaveJSProperty('disabled',false);
  expect(await page.evaluate(()=>(window as any).__testRequests.filter((r:any)=>r.command!=='status'))).toEqual([]);
  await page.evaluate(()=>(window as any).__pickerError=true);
  await page.locator('#activity-projects').click();
  await expect(page.locator('#error')).toHaveText('Folder picker unavailable');
  expect(await page.evaluate(()=>(window as any).__testState.projects.length)).toBe(9);
});

test('re-adding a disabled project preserves its existing goal',async({page})=>{
  await setup(page);
  await page.evaluate(()=>{const w=window as any;w.__testState.projects[0].enabled=false;w.__testState.settings.project_scope.splice(0,1);w.__pickedProjects=['/projects/app-1'];});
  await page.locator('#activity-projects').click();
  await expect.poll(()=>page.evaluate(()=>(window as any).__testRequests.filter((r:any)=>r.command==='project').length)).toBe(1);
  const added=await page.evaluate(()=>(window as any).__testRequests.find((r:any)=>r.command==='project'));
  expect(added.objective).toBe('Build the application');
  expect(await page.evaluate(()=>(window as any).__testState.projects.length)).toBe(9);
});

 test('older services disable controls with restart guidance, then recover',async({page})=>{
  await setup(page);
  await page.evaluate(()=>(window as any).__testState.protocol_version=1);
  await expect(page.locator('#connection')).toHaveText('Disconnected');
  await expect(page.locator('#error')).toHaveText('Restart the updated Prodex service to use these settings.');
  await expect(page.locator('#activity-projects')).toHaveJSProperty('disabled',true);
  await page.evaluate(()=>(window as any).__testState.protocol_version=8);
  await expect(page.locator('#connection')).toHaveText('Connected');
  await expect(page.locator('#error')).toBeHidden();
  await expect(page.locator('#activity-projects')).toHaveJSProperty('disabled',false);
});


test('activity groups workers separately from observed Codex sessions and offers review actions',async({page})=>{
  await setup(page);
  await expect(page.locator('#activity-tab')).toHaveAttribute('aria-selected','true');
  await expect(page.getByRole('img',{name:'No session',exact:true})).toHaveCount(9);
  await page.evaluate(()=>{
    const state=(window as any).__testState;
    state.observation.sessions=[{id:'main-session',cwd:'/projects/app-1',project:'/projects/app-1',last_seen:1700000000,source:'process'}];
    state.tasks=['awaiting_approval','running','succeeded','failed','interrupted','recovery_required'].map((status,i)=>({id:`task-${i}`,status,review:'not_required',session_id:i===2?'worker-session':null,worktree:'/worktrees/task',created_at:1700000000,updated_at:1700000010+i,summary:'Useful result',proposal:{project:i===3?'/projects/app-2':'/projects/app-1',prompt:`Task ${i}`,rationale:'Next natural step',provider:i%2?'claude':'codex',mode:'read_only',risk:'low',completion_criteria:'Explain findings'}}));
  });
  await expect(page.locator('.task')).toHaveCount(5);
  await page.getByRole('button',{name:'Show completed (1)',exact:true}).click();
  await expect(page.locator('.task')).toHaveCount(6);
  await expect(page.locator('.project-header .session-indicator[data-live=true]')).toHaveAttribute('title','Live code session');
  await expect(page.locator('[data-project="/projects/app-2"] .task')).toHaveCount(1);
  const pending=page.locator('[data-id="task-0"]');await pending.locator('summary').click();
  await expect(pending).not.toContainText('Next natural step');await expect(pending).not.toContainText('/worktrees/task');
  await pending.getByText('Approve',{exact:true}).click();await expect(pending.getByRole('img',{name:'Queued',exact:true})).toBeVisible();
  await page.locator('[data-id="task-1"] summary').click();await page.locator('[data-id="task-1"]').getByText('Stop task',{exact:true}).click();
  await expect(page.locator('[data-id="task-1"]').getByRole('img',{name:'Stopped',exact:true})).toBeVisible();
  await page.locator('[data-id="task-2"] summary').click();await expect(page.locator('[data-id="task-2"]')).not.toContainText('Useful result');
  await page.screenshot({path:'/tmp/prodex-activity.png'});
});

test('legacy null scope never opts folders in and unavailable observation is visible',async({page})=>{
  await setup(page);await page.evaluate(()=>{const s=(window as any).__testState;s.settings.project_scope=null;s.observation.available=false;s.observation.detail='Process inspection unavailable';});
  await expect(page.locator('#observation-status')).toContainText('Observation unavailable');
  await expect(page.locator('.project-activity')).toHaveCount(0);
  expect(await page.evaluate(()=>(window as any).__testRequests.filter((r:any)=>r.command==='set_activation'))).toEqual([]);
  await expect(page.locator('#scope-menu')).toHaveCount(0);
});


test('activity refreshes while a task has keyboard focus and keeps details open',async({page})=>{
  await setup(page);await page.evaluate(()=>{
    (window as any).__testState.tasks=[{id:'focused-task',status:'running',review:'not_required',session_id:null,worktree:null,created_at:1700000000,updated_at:1700000000,summary:'Initial',proposal:{project:'/projects/app-1',prompt:'Focused task',rationale:'A reason',provider:'codex',mode:'read_only',risk:'low',completion_criteria:''}}];
  });
  const card=page.locator('[data-id="focused-task"]');await expect(card).toBeVisible();await card.locator('summary').click();await card.locator('summary').focus();
  await page.evaluate(()=>(window as any).__testState.tasks[0].status='failed');
  await expect(card.getByRole('img',{name:'Needs retry',exact:true})).toBeVisible();await expect(card).toHaveJSProperty('open',true);await expect(card.locator('summary')).toBeFocused();
});


test('activity hides process details and shows compact durations, latest first, and planner waiting',async({page})=>{
  await setup(page);
  await page.evaluate(()=>{
    const s=(window as any).__testState;const now=Math.floor(Date.now()/1000);
    s.settings.planning_enabled=true;s.settings.paused=false;
    s.observation.sessions=[1,2].map(i=>({id:`codex-process:${i}:technical-timestamp`,cwd:'/projects/app-1',project:'/projects/app-1',last_seen:now,source:'process'}));
    s.planning_activity=[{project:'/projects/app-1',message:'Previous check stopped',next_check_at:now+180}];
    s.tasks=[1,2].map(i=>({id:`compact-${i}`,status:'running',started_at:now-120,review:'not_required',created_at:now-900,updated_at:now+i,summary:'',proposal:{project:'/projects/app-1',prompt:`Improve parser ${i}. A long implementation instruction follows.`,rationale:'Independent work',provider:'codex',mode:'read_only',risk:'low'}}));
  });
  const project=page.locator('[data-project="/projects/app-1"]');
  await expect(project.locator('.session-indicator')).toHaveAttribute('title','Live code session');
  await expect(project).not.toContainText('codex-process');
  await expect(project).not.toContainText('/projects/app-1');
  await expect(project.locator('.planning-status')).toContainText('Next check in');
  await expect(project.locator('.task').first()).toHaveAttribute('data-id','compact-2');
  await expect(project.locator('.task').first().locator('summary')).toHaveText('Improve parser 2.2m');
  await page.screenshot({path:'/tmp/prodex-compact-activity.png'});
});

async function copyFixture(page:Page,provider:string,status:string,session:string|null) {
  await page.evaluate(({provider,status,session})=>{
    const s=(window as any).__testState;
    s.tasks=[{id:'copy-task',status,review:'not_required',session_id:session,worktree:null,created_at:1700000000,updated_at:1700000000,summary:'Findings',proposal:{project:'/projects/app-1',prompt:'Investigate the parser',rationale:'Independent research',provider,mode:'read_only',risk:'low'}}];
  },{provider,status,session});
  if(status==='succeeded')await page.getByRole('button',{name:'Show completed (1)',exact:true}).click();
  const card=page.locator('[data-id="copy-task"]');
  await expect(card).toBeVisible();await card.locator('summary').click();return card;
}

test('unstarted suggestions copy their prompt without creating a session',async({page})=>{
  await setup(page);const card=await copyFixture(page,'codex','awaiting_approval',null);
  await expect(card.getByRole('button',{name:'Copy session ID'})).toHaveCount(0);
  await expect(card).toContainText('A session is created when this suggestion runs.');
  await card.getByRole('button',{name:'Copy prompt'}).click();
  await expect.poll(()=>page.evaluate(()=>(window as any).__clipboardText)).toBe('Investigate the parser');
  await expect(card.locator('wa-button[data-action=copy-prompt]')).toHaveText('Copied');
  expect(await page.evaluate(()=>(window as any).__testRequests.filter((r:any)=>r.command!=='status'))).toEqual([]);
});

test('finished Codex and Claude sessions copy quoted resume commands',async({page})=>{
  await setup(page);const card=await copyFixture(page,'codex','succeeded','codex-session');
  await page.evaluate(()=>(window as any).__testState.tasks[0].worktree="/worktrees/it's $(echo nope)");
  await card.getByRole('button',{name:'Copy command'}).click();
  const quote=(value:string)=>"'"+value.replaceAll("'", "'\\''")+"'";
  await expect.poll(()=>page.evaluate(()=>(window as any).__clipboardText)).toBe(`cd -- ${quote("/worktrees/it's $(echo nope)")} && codex resume --include-non-interactive 'codex-session'`);
  await expect(card.getByRole('button',{name:'Copy session ID'})).toHaveCount(0);
  await page.evaluate(()=>{const t=(window as any).__testState.tasks[0];t.proposal.provider='claude';t.session_id='claude-session';t.worktree=null;});
  await card.getByRole('button',{name:'Copy command'}).click();
  await expect.poll(()=>page.evaluate(()=>(window as any).__clipboardText)).toBe("cd -- '/projects/app-1' && claude --resume 'claude-session'");
});

test('running sessions offer full command copying and Terminal access',async({page})=>{
  await setup(page);const card=await copyFixture(page,'codex','running','known-session');
  await expect(card.getByRole('button',{name:'Copy session ID'})).toHaveCount(0);
  await card.getByRole('button',{name:'Copy command'}).click();
  await expect.poll(()=>page.evaluate(()=>(window as any).__clipboardText)).toBe("cd -- '/projects/app-1' && codex resume --include-non-interactive 'known-session'");
  await card.getByRole('button',{name:'Open in Terminal'}).click();
  await expect.poll(()=>page.evaluate(()=>(window as any).__openedSession)).toBe('copy-task');
  await page.evaluate(()=>(window as any).__copyFailure=true);
  await card.getByRole('button',{name:'Copy command'}).click();
  await expect(page.locator('#error')).toContainText('Could not copy');
});


test('long reports stay out of the card and Open in Terminal is immediately accessible',async({page})=>{
  await setup(page);const card=await copyFixture(page,'codex','succeeded','finished-session');
  await page.evaluate(()=>{const t=(window as any).__testState.tasks[0];t.summary='## A huge report with tables '.repeat(1000);t.proposal.rationale='A huge rationale '.repeat(1000);});
  await page.locator('#settings-tab').click();await page.locator('#activity-tab').click();
  await expect(card).not.toContainText('A huge report');
  await expect(card).not.toContainText('A huge rationale');
  const button=card.getByRole('button',{name:'Open in Terminal'});
  await expect(button).toBeVisible();
  await expect.poll(async()=> (await card.boundingBox())!.height).toBeLessThan(320);
  await button.click();
  await expect.poll(()=>page.evaluate(()=>(window as any).__openedSession)).toBe('copy-task');
  await expect(card.locator('wa-button[data-action=open-session]')).toHaveText('Opened');
  await page.screenshot({path:'/tmp/prodex-session-actions.png'});
  await page.evaluate(()=>(window as any).__openFailure=true);
  await button.click();await expect(page.locator('#error')).toContainText('Could not open Terminal');
  await expect(card.getByRole('button',{name:'Copy command'})).toBeVisible();
});


test('PR brief shows intent, approach and verification with accessible status colors',async({page})=>{
  await setup(page);const card=await copyFixture(page,'codex','awaiting_approval',null);
  await page.evaluate(()=>{const t=(window as any).__testState.tasks[0];t.proposal.brief={title:'Keep cancellation responsive while provider output is busy',change:'Allow Stop to interrupt a task even when its output queue is full.',approach:'Separate output delivery from process supervision. Add a regression test that fills the queue before cancellation.'};t.proposal.completion_criteria='The backpressure regression test passes and the worker exits promptly.';});
  await expect(card.locator('.task-title')).toHaveText('Keep cancellation responsive while provider output is busy');
  await expect(card.getByRole('img',{name:'Needs approval'})).toBeVisible();
  await expect(card.locator('.task-meta')).toBeEmpty();
  await expect(card.getByRole('heading',{name:'Proposed change'})).toBeVisible();
  await expect(card.getByRole('heading',{name:'Approach'})).toBeVisible();
  await expect(card.getByRole('heading',{name:'Verification'})).toBeVisible();
  await expect(card).toContainText('Separate output delivery');
  await expect(card.getByRole('button',{name:'Approve',exact:true})).toBeVisible();
  await page.screenshot({path:'/tmp/prodex-pr-brief.png'});
  await page.setViewportSize({width:540,height:620});
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
});

test('completed and rejected ideas stay hidden until history is expanded',async({page})=>{
  await setup(page);const card=await copyFixture(page,'codex','awaiting_approval',null);
  const project=page.locator('[data-project="/projects/app-1"]');
  await card.getByRole('button',{name:'Reject',exact:true}).click();
  await expect(card).toHaveCount(0);
  const toggle=project.getByRole('button',{name:'Show rejected (1)',exact:true});
  await expect(toggle).toBeVisible();await expect(toggle).toBeFocused();
  await toggle.click();await expect(card).toBeVisible();
  await card.locator('summary').click();
  await expect(card.getByRole('img',{name:'Rejected',exact:true})).toBeVisible();
  await expect(card.getByRole('button',{name:'Approve',exact:true})).toHaveCount(0);
  await project.getByRole('button',{name:'Hide rejected (1)'}).click();
  await expect(card).toHaveCount(0);
  await page.evaluate(()=>{const s=(window as any).__testState;const t=structuredClone(s.tasks[0]);t.id='completed';t.status='succeeded';s.tasks.push(t);});
  const combined=project.getByRole('button',{name:'Show completed and rejected (2)'});
  await expect(combined).toBeVisible();
  await expect(project.locator('.task')).toHaveCount(0);
  await combined.click();await expect(project.locator('.task')).toHaveCount(2);
  await project.getByRole('button',{name:'Hide completed and rejected (2)'}).click();
  await page.waitForTimeout(2200);
  await expect(project.locator('.task')).toHaveCount(0);await expect(combined).toBeFocused();
});

test('title, status, duration and arrow align when toggled and resized',async({page})=>{
  await setup(page);
  await page.evaluate(()=>{
    const s=(window as any).__testState;const now=Math.floor(Date.now()/1000);
    s.projects=s.projects.slice(0,1);
    s.tasks=['awaiting_approval','running','succeeded','failed','rejected'].map((status,i)=>({id:`aligned-${i}`,status,started_at:now-180,created_at:now-300,updated_at:now,session_id:i===2?'finished-session':null,worktree:null,proposal:{project:'/projects/app-1',provider:'codex',prompt:'Implement the parser improvement.',completion_criteria:'Regression tests pass.',brief:{title:i===1?'Keep provider cancellation responsive when the output queue is full and the worker is still producing events':'Handle empty parser input',change:'Return a clear error for empty input.',approach:'Update the parser and add a regression test.'}}}));
  });
  await expect(page.locator('.task')).toHaveCount(3);
  for(const width of [680,540]) {
    await page.setViewportSize({width,height:width===680?760:620});
    const card=page.locator('[data-id="aligned-1"]');
    let closedX=0;
    for(const open of [false,true]) {
      if(open)await card.locator('summary').click();
      const geometry=await card.evaluate(node=>{
        const box=(selector:string)=>node.querySelector(selector)!.getBoundingClientRect();
        const title=box('.task-title'), icon=box('.status-icon'), arrow=box('.task-chevron'), age=box('.task-meta');
        const line=parseFloat(getComputedStyle(node.querySelector('.task-title')!).lineHeight);
        return {titleX:title.x,lineCenter:title.y+line/2,iconCenter:icon.y+icon.height/2,arrowCenter:arrow.y+arrow.height/2,ageCenter:age.y+age.height/2,ageRight:age.right,arrowLeft:arrow.left,arrowRight:arrow.right,cardRight:node.getBoundingClientRect().right};
      });
      expect(Math.abs(geometry.iconCenter-geometry.lineCenter)).toBeLessThan(1);
      expect(Math.abs(geometry.arrowCenter-geometry.lineCenter)).toBeLessThan(1);
      expect(Math.abs(geometry.ageCenter-geometry.lineCenter)).toBeLessThan(1);
      expect(geometry.ageRight).toBeLessThan(geometry.arrowLeft);
      expect(geometry.arrowRight).toBeLessThan(geometry.cardRight);
      if(open)expect(geometry.titleX).toBe(closedX);else closedX=geometry.titleX;
      await page.screenshot({path:`/tmp/prodex-alignment-${width}-${open?'open':'closed'}.png`});
    }
    await card.locator('summary').click();
  }
  await page.setViewportSize({width:680,height:760});
  await page.getByRole('button',{name:'Show completed and rejected (2)'}).click();
  await expect(page.locator('[data-id="aligned-4"]')).toBeVisible();
  await page.screenshot({path:'/tmp/prodex-expanded-history.png'});
});


test('Check now is beside the countdown and sends an explicit project check',async({page})=>{
  await setup(page);await page.evaluate(()=>{const s=(window as any).__testState;s.settings.planning_enabled=true;s.settings.paused=false;s.observation.sessions=[{project:'/projects/app-1',id:'live'}];s.planning_activity=[{project:'/projects/app-1',message:'Waiting',next_check_at:Math.floor(Date.now()/1000)+60}];});
  const button=page.getByRole('button',{name:'Check now',exact:true});await expect(button).toBeVisible();
  const row=page.locator('[data-project="/projects/app-1"] .planning-row');
  await expect(row).toContainText('Next check in');
  await button.click();await expect(row).toContainText('Looking for work');await expect(button).toHaveCount(0);
  expect(await page.evaluate(()=>(window as any).__testRequests.filter((r:any)=>r.command==='check_now'))).toEqual([{command:'check_now',project:'/projects/app-1'}]);
});

test('projects collapse, reorder by dragging or keyboard, and remember layout',async({page})=>{
  await setup(page);
  const first=page.locator('[data-project="/projects/app-1"]');
  await first.locator('.project-toggle').click();await expect(first.locator('.project-content')).toBeHidden();
  await page.getByRole('button',{name:'Reorder app-2',exact:true}).dragTo(first.locator('.project-header'));
  await expect(page.locator('.project-activity').first()).toHaveAttribute('data-project','/projects/app-2');
  await page.getByRole('button',{name:'Reorder app-3',exact:true}).focus();await page.keyboard.press('ArrowUp');
  await expect(page.locator('.project-activity').nth(1)).toHaveAttribute('data-project','/projects/app-3');
  await page.reload();
  await expect(page.locator('.project-activity').first()).toHaveAttribute('data-project','/projects/app-2');
  await expect(first.locator('.project-content')).toBeHidden();
  const alignment=await first.locator('.project-toggle').evaluate(node=>({left:node.getBoundingClientRect().left,text:node.querySelector('span:last-child')!.getBoundingClientRect().left}));
  expect(alignment.text-alignment.left).toBeLessThan(30);
  await page.screenshot({path:'/tmp/prodex-project-layout.png'});
});

test('unconnecting requires confirmation, handles failure, and can be reconnected',async({page})=>{
  await setup(page);const first=page.locator('[data-project="/projects/app-1"]');
  await first.getByRole('button',{name:'Unconnect app-1',exact:true}).click();
  const dialog=page.getByRole('dialog');await expect(dialog).toBeVisible();await expect(dialog).toContainText('Your files, worktrees, and task history are kept');
  await dialog.getByRole('button',{name:'Cancel',exact:true}).click();await expect(first).toBeVisible();
  await first.getByRole('button',{name:'Unconnect app-1',exact:true}).click();
  await page.evaluate(()=>(window as any).__failNext=true);
  await dialog.getByRole('button',{name:'Unconnect project',exact:true}).click();await expect(dialog.getByRole('alert')).toContainText('Could not save');await expect(first).toBeVisible();
  await page.screenshot({path:'/tmp/prodex-unconnect-dialog.png'});
  await dialog.getByRole('button',{name:'Unconnect project',exact:true}).click();await expect(dialog).toBeHidden();await expect(first).toHaveCount(0);
  await page.evaluate(()=>(window as any).__pickedProjects=['/projects/app-1']);await page.getByRole('button',{name:'Add projects',exact:true}).click();await expect(first).toBeVisible();
});


test('daily planning cap is distinguished from cooldown and cannot offer Check now',async({page})=>{
  await setup(page);await page.evaluate(()=>{const s=(window as any).__testState;s.settings.planning_enabled=true;s.settings.paused=false;s.observation.sessions=[{project:'/projects/app-1',id:'live'}];s.planning_activity=[{project:'/projects/app-1',message:'Daily planning limit reached',next_check_at:Math.floor(Date.now()/1000)+36000,daily_limit_reached:true}];});
  const status=page.locator('[data-project="/projects/app-1"] .planning-status');
  await expect(status).toContainText('Daily check limit · resets in');
  await expect(status).not.toContainText('Next check in');
  await expect(page.getByRole('button',{name:'Check now',exact:true})).toHaveCount(0);
  await page.evaluate(()=>{const p=(window as any).__testState.planning_activity[0];p.daily_limit_reached=false;p.message='No new work found';p.next_check_at=Math.floor(Date.now()/1000)+60;});
  await expect(status).toContainText('Next check in');
  await expect(page.getByRole('button',{name:'Check now',exact:true})).toBeVisible();
});


test('planning interval and daily limit save, validate, and fit the small window',async({page})=>{
  await setup(page);await page.locator('#settings-tab').click();
  await expect(page.getByLabel('Check interval',{exact:true})).toHaveValue('60');
  const daily=page.getByLabel('Daily check limit',{exact:true});await expect(daily).toHaveValue('1440');
  await page.getByLabel('Check interval',{exact:true}).selectOption('120');
  await expect.poll(()=>page.evaluate(()=>(window as any).__testState.settings.planner_cooldown_secs)).toBe(120);
  await daily.fill('240');await daily.press('Tab');
  await expect.poll(()=>page.evaluate(()=>(window as any).__testState.settings.max_plans_per_day)).toBe(240);
  await daily.fill('0');await daily.press('Tab');
  await expect(page.locator('#error')).toContainText('positive whole number');
  expect(await page.evaluate(()=>(window as any).__testState.settings.max_plans_per_day)).toBe(240);
  await daily.fill('1440');await daily.press('Tab');await expect(page.locator('#error')).toBeHidden();
  await page.setViewportSize({width:540,height:620});await noScroll(page);
  const bounds=await page.locator('#config-form').boundingBox();
  expect(bounds!.y+bounds!.height).toBeLessThan(620);
  await page.screenshot({path:'/tmp/prodex-planner-settings.png'});
});


test('failed attempts show an actionable toast and can be explicitly retried',async({page})=>{
  await setup(page);const card=await copyFixture(page,'codex','needs_retry',null);
  await page.evaluate(()=>{const t=(window as any).__testState.tasks[0];t.summary='Workspace preparation failed: fatal: not a git repository';t.updated_at++;});
  await expect(page.locator('#toast')).toContainText('Git repository with an initial commit');
  await expect(card.getByRole('img',{name:'Needs retry'})).toBeVisible();
  await expect(card.locator('.task-retry-reason')).toContainText('Git repository');
  await page.screenshot({path:'/tmp/prodex-retry-notification.png'});
  await page.getByRole('button',{name:'Dismiss notification'}).click();
  await page.waitForTimeout(2200);await expect(page.locator('#toast')).toBeHidden();
  await card.getByRole('button',{name:'Try again',exact:true}).click();
  await expect(card.getByRole('img',{name:'Queued',exact:true})).toBeVisible();
  expect(await page.evaluate(()=>(window as any).__testRequests.filter((r:any)=>r.command==='retry'))).toEqual([{command:'retry',id:'copy-task'}]);
});


test('Git setup is first and coding waits until its commit is verified',async({page})=>{
  await setup(page);
  await page.evaluate(()=>{
    const state=(window as any).__testState;
    const base={review:'not_required',summary:'',created_at:1700000000,updated_at:1700000010,session_id:null,worktree:null};
    const proposal={project:'/projects/app-1',provider:'codex',risk:'medium',rationale:'Useful work',completion_criteria:'Verify result'};
    state.tasks=[
      {...base,id:'coding',status:'awaiting_approval',proposal:{...proposal,mode:'edit',prompt:'Improve the parser',dependencies:['git-setup']}},
      {...base,updated_at:1700000000,id:'git-setup',status:'awaiting_approval',proposal:{...proposal,mode:'initialize_repository',prompt:'Create a Git repository and initial commit'}}
    ];
  });
  const setupCard=page.locator('details[data-id="git-setup"]');
  const coding=page.locator('details[data-id="coding"]');
  await expect(page.locator('details.task').first()).toHaveAttribute('data-id','git-setup');
  await setupCard.locator('summary').click();
  await expect(setupCard.getByText('Runs directly in this project folder',{exact:false})).toBeVisible();
  await expect(setupCard.getByText('Approve',{exact:true})).toBeVisible();
  await coding.locator('summary').click();
  await expect(coding.getByText('Complete Git setup first.',{exact:false})).toBeVisible();
  await expect(coding.getByText('Approve',{exact:true})).toHaveCount(0);
  await page.screenshot({path:'/tmp/prodex-git-setup.png'});
  await page.evaluate(()=>{(window as any).__testState.tasks.find((t:any)=>t.id==='git-setup').status='succeeded';});
  await expect(coding.getByText('Approve',{exact:true})).toBeVisible();
  await expect(coding.getByText('Complete Git setup first.',{exact:false})).toHaveCount(0);
});

test('workspace choice is per project, confirmed and preserved through polling',async({page})=>{
  await setup(page);await page.locator('#settings-tab').click();await page.locator('#workspaces-page').click();
  await expect(page.locator('#workspace-mode')).toHaveValue('worktree');
  await page.locator('#workspace-mode').selectOption('main');await expect(page.locator('#choice-dialog')).toBeVisible();
  await page.locator('#choice-cancel').click();expect(await page.evaluate(()=>(window as any).__testRequests.some((r:any)=>r.command==='set_worktrees'))).toBe(false);
  await page.locator('#workspace-mode').selectOption('main');await page.locator('#choice-confirm').click();
  await expect.poll(()=>page.evaluate(()=>(window as any).__testState.projects[0].use_worktrees)).toBe(false);
  await expect(page.locator('#workspace-mode')).toHaveValue('main');
  await page.locator('#workspace-project').selectOption('/projects/app-2');await expect(page.locator('#workspace-mode')).toHaveValue('worktree');
  await page.locator('#workspace-project').selectOption('/projects/app-1');await expect(page.locator('#workspace-mode')).toHaveValue('main');
  await page.screenshot({path:'/tmp/prodex-main-folder-settings.png'});
});

test('completion order can be dragged or moved with buttons and is remembered',async({page})=>{
  await setup(page);await page.locator('#settings-tab').click();await page.locator('#completion-page').click();
  const rows=page.locator('#completion-order [role=listitem]');await expect(rows.first()).toHaveAttribute('data-completion-action','merge');
  await page.locator('[data-completion-action=terminal]').dragTo(page.locator('[data-completion-action=merge]'));
  await expect(rows.first()).toHaveAttribute('data-completion-action','terminal');
  await expect(page.getByRole('button',{name:'Create PR',exact:true})).toHaveCount(0);
  expect(await page.evaluate(()=>JSON.parse(localStorage.getItem('prodex.completionOrder')!))).toEqual(['terminal','merge']);
  await page.reload();await page.locator('#settings-tab').click();await page.locator('#completion-page').click();
  await expect(rows.first()).toHaveAttribute('data-completion-action','terminal');
  await expect(page.locator('#completion-page')).toHaveAttribute('aria-pressed','true');
  await page.screenshot({path:'/tmp/prodex-completion-order.png'});
});

test('finished worktree merges in the background and disappears only after verification',async({page})=>{
  await setup(page);const card=await copyFixture(page,'codex','succeeded','finished-session');
  await page.evaluate(()=>{const t=(window as any).__testState.tasks[0];t.proposal.mode='edit';t.worktree='/worktrees/task';t.review='awaiting_review';});
  await expect(card.getByRole('button',{name:'Merge locally',exact:true})).toBeVisible();
  await card.getByRole('button',{name:'Merge locally',exact:true}).click();
  await expect(card).toContainText('Merging locally');
  expect(await page.evaluate(()=>(window as any).__openedSession)).toBeNull();
  await expect(card.getByRole('button',{name:'Stop merge'})).toBeVisible();
  await expect(card.getByRole('button',{name:'Mark as integrated…'})).toHaveCount(0);
  await expect(page.locator('[data-id="merge-job"]')).toHaveCount(0);
  await page.evaluate(()=>{const tasks=(window as any).__testState.tasks;tasks.find((t:any)=>t.id==='merge-job').status='succeeded';tasks.find((t:any)=>t.id==='copy-task').review='integrated';});
  // History was expanded by the initial fixture; close it to check normal active view.
  const history=page.getByRole('button',{name:/Hide completed/});
  await expect(history).toBeVisible();await history.click();
  await expect(card).toHaveCount(0);
});

test('main-folder result needs review but has no merge or PR actions',async({page})=>{
  await setup(page);
  await page.evaluate(()=>{(window as any).__testState.tasks=[{id:'direct-code',status:'succeeded',review:'awaiting_review',session_id:'session',worktree:null,created_at:1700000000,updated_at:1700000000,summary:'Done',proposal:{project:'/projects/app-1',provider:'claude',mode:'edit_in_place',prompt:'Keep settings after restart',rationale:'Preserve preferences',completion_criteria:'Restart retains preferences'}}];});
  const card=page.locator('[data-id=direct-code]');await expect(card).toBeVisible();await card.locator('summary').click();
  await expect(card.getByText('Merge locally',{exact:true})).toHaveCount(0);await expect(card.getByText('Create PR',{exact:true})).toHaveCount(0);
  await expect(card.getByText('Open in Terminal',{exact:true})).toBeVisible();
  await card.getByText('Mark reviewed',{exact:true}).click();await page.locator('#choice-confirm').click();await expect(card).toHaveCount(0);
});


test('destination selection is remembered and filters incompatible provider apps',async({page})=>{
  await setup(page);const card=await copyFixture(page,'codex','running','codex-session');
  const picker=card.getByRole('combobox',{name:'Open session with'});
  await expect(picker.locator('option[value=claude]')).toHaveCount(0);
  await picker.selectOption('ghostty');
  await card.getByRole('button',{name:'Open in Ghostty'}).click();
  await expect.poll(()=>page.evaluate(()=>(window as any).__openedDestination)).toBe('ghostty');
  await page.reload();
  const restored=await copyFixture(page,'codex','running','codex-session');
  await expect(restored.getByRole('button',{name:'Open in Ghostty'})).toBeVisible();
  await restored.getByRole('combobox',{name:'Open session with'}).selectOption('codex');
  await page.evaluate(()=>{(window as any).__testState.tasks[0].proposal.provider='claude';});
  await expect(restored.getByRole('button',{name:'Open in Terminal'})).toBeVisible();
  expect(await page.evaluate(()=>localStorage.getItem('prodex.openDestination'))).toBe('codex');
});

test('editor handoff explains copied command and custom app can be selected',async({page})=>{
  await setup(page);const card=await copyFixture(page,'codex','running','codex-session');
  await card.getByRole('combobox',{name:'Open session with'}).selectOption('zed');
  await page.evaluate(()=>(window as any).__openNotice='Opened Zed. Resume command copied—paste it in the integrated terminal.');
  await card.getByRole('button',{name:'Open in Zed'}).click();
  await expect(card).toContainText('paste it in the integrated terminal');
  await page.screenshot({path:'/tmp/prodex-open-destinations.png'});
  await page.evaluate(()=>(window as any).__pickedDestination={id:'app:/Applications/My Terminal.app',label:'My Terminal',kind:'clipboard'});
  await card.getByRole('combobox',{name:'Open session with'}).selectOption('other');
  await expect(card.getByRole('button',{name:'Open in My Terminal'})).toBeVisible();
  await card.getByRole('button',{name:'Open in My Terminal'}).click();
  await expect.poll(()=>page.evaluate(()=>(window as any).__openedDestination)).toBe('app:/Applications/My Terminal.app');
});


test('missing saved apps fall back without losing the preference and picker cancel is harmless',async({page})=>{
  await page.addInitScript(()=>localStorage.setItem('prodex.openDestination','app:/missing/Terminal.app'));
  await setup(page);const card=await copyFixture(page,'codex','running','session');
  await expect(card.getByRole('button',{name:'Open in Terminal'})).toBeVisible();
  await card.getByRole('combobox',{name:'Open session with'}).selectOption('other');
  await expect(card.getByRole('button',{name:'Open in Terminal'})).toBeVisible();
  expect(await page.evaluate(()=>localStorage.getItem('prodex.openDestination'))).toBe('app:/missing/Terminal.app');
  expect(await page.evaluate(()=>(window as any).__openedSession)).toBeNull();
});


test('empty projects show one planning message instead of duplicate empty labels',async({page})=>{
  await setup(page);
  await page.evaluate(()=>{const s=(window as any).__testState;s.planning_activity=[{project:'/projects/app-1',message:'Looking for work…',next_check_at:null}];});
  const project=page.locator('[data-project="/projects/app-1"]');
  await expect(project).toContainText('Looking for work…');
  await expect(project).not.toContainText('No current ideas');
  await expect(project).not.toContainText('No Prodex sessions yet');
});


test('shows three ranked ideas and promotes the reserve after approval or rejection',async({page})=>{
  await setup(page);
  await page.evaluate(()=>{
    const s=(window as any).__testState;
    s.tasks=Array.from({length:10},(_,i)=>({id:`idea-${i}`,status:'awaiting_approval',review:'not_required',created_at:1700000000,updated_at:1700000000+i,
      proposal:{project:'/projects/app-1',provider:'codex',mode:'edit',prompt:`Useful outcome ${i}`,rationale:'Reach the product goal',completion_criteria:'Acceptance tests pass'}}));
    s.tasks.push({id:'active',status:'running',created_at:1700000000,updated_at:1700001000,proposal:{project:'/projects/app-1',provider:'codex',mode:'edit',prompt:'Current work'}});
  });
  const group=page.locator('[data-project="/projects/app-1"]');
  await expect(group.locator('details.task')).toHaveCount(4);
  await expect(group.locator('details.task .task-title')).toHaveText(['Current work','Useful outcome 0','Useful outcome 1','Useful outcome 2']);
  await expect(group).toContainText('7 more in reserve');
  await group.locator('[data-id="idea-0"] summary').click();
  await group.locator('[data-id="idea-0"]').getByRole('button',{name:'Approve',exact:true}).click();
  await expect(group.locator('[data-id="idea-3"]')).toBeVisible();
  await expect(group.locator('[data-id="idea-4"]')).toHaveCount(0);
  await group.locator('[data-id="idea-1"] summary').click();
  await group.locator('[data-id="idea-1"]').getByRole('button',{name:'Reject',exact:true}).click();
  await expect(group.locator('[data-id="idea-4"]')).toBeVisible();
  await expect(group.locator('[data-id="idea-1"]')).toHaveCount(0);
  await expect(group.locator('[data-id="active"]')).toBeVisible();
  await expect(group.locator('[data-id="idea-0"]')).toBeVisible();
});


test('blocked merge resumes the merge session and lets users dismiss without deleting files',async({page})=>{
  await setup(page);const card=await copyFixture(page,'codex','succeeded','coding-session');
  await page.evaluate(()=>{
    const s=(window as any).__testState,t=s.tasks[0];
    t.proposal.mode='edit';t.worktree='/worktrees/task';t.review='awaiting_review';
    s.tasks.push({...structuredClone(t),id:'blocked-merge',status:'needs_retry',session_id:'merge-session',summary:'Integration blocked: main has overlapping edits in src/app.rs',proposal:{...t.proposal,mode:'merge',dependencies:[t.id]}});
  });
  await expect(card).toContainText('main has overlapping edits');
  await expect(card.getByRole('button',{name:'Retry merge',exact:true})).toBeVisible();
  await card.getByRole('button',{name:'Resolve in Terminal',exact:true}).click();
  expect(await page.evaluate(()=>(window as any).__openedSession)).toBe('blocked-merge');
  await card.getByRole('button',{name:'Copy command',exact:true}).click();
  expect(await page.evaluate(()=>(window as any).__clipboardText)).toContain('merge-session');
  await card.getByRole('button',{name:'Dismiss result',exact:true}).click();
  const dialog=page.getByRole('dialog');await expect(dialog).toContainText('Nothing is merged, undone or deleted');
  await dialog.getByRole('button',{name:'Dismiss result',exact:true}).click();
  const history=page.getByRole('button',{name:/Hide rejected/});
  if(await history.isVisible())await history.click();
  await expect(card).toHaveCount(0);
  expect(await page.evaluate(()=>(window as any).__testState.tasks[0].worktree)).toBe('/worktrees/task');
  expect(await page.evaluate(()=>(window as any).__testState.tasks[0].review)).toBe('awaiting_review');
});
