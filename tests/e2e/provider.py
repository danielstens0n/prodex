#!/usr/bin/env python3
"""Deterministic CLI protocol stand-in; never invokes an LLM."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import uuid

prompt = sys.stdin.read()
claude = Path(sys.argv[0]).name == 'claude'
session = str(uuid.uuid4())
def emit(value):
    print(json.dumps(value), flush=True)
def finish(text, success=True):
    if claude:
        emit(dict(type='result', subtype='success' if success else 'error', is_error=not success, result=text, session_id=session))
    else:
        emit(dict(type='item.completed', item=dict(type='agent_message', text=text)))
        emit(dict(type='turn.completed') if success else dict(type='turn.failed', error=dict(message=text)))
def git(*args):
    return subprocess.check_output(['git', *args], text=True, stderr=subprocess.STDOUT).strip()

emit(dict(type='system', subtype='init', session_id=session) if claude else dict(type='thread.started', thread_id=session))
if 'maximum_new_proposals' in prompt:
    proposals = [dict(brief=dict(title=f'Build fixture feature {i}', change='Complete a fixture user journey', approach='Implement the scoped feature'), prompt=f'E2E_RANKED_{i}', rationale='Documented fixture goal', completion_criteria='Feature exists', mode='edit', expected_files=[f'feature-{i}.txt'], dependencies=[], risk='low') for i in range(10)]
    finish(json.dumps(dict(project_notes='Fixture goal and ranked opportunities', proposals=proposals)))
    sys.exit(0)
if 'E2E_MALFORMED' in prompt:
    print('not-json', flush=True)
    sys.exit(0)
if 'E2E_SLOW' in prompt:
    time.sleep(60)
if 'E2E_FAIL_ONCE' in prompt:
    marker = Path(os.environ['E2E_FAILURE_MARKER'])
    if not marker.exists():
        marker.write_text('failed')
        finish('Fixture provider failed once', False)
        sys.exit(1)
if '--add-dir' in sys.argv:
    project = sys.argv[sys.argv.index('--add-dir') + 1]
    if Path(project, 'BLOCK_MERGE').exists():
        finish('Integration blocked: overlapping main edits need attention')
        sys.exit(0)
    git('add', 'feature.txt')
    git('commit', '-m', 'Implement fixture feature')
    branch = git('branch', '--show-current')
    subprocess.run(['git', '-C', project, 'merge', '--no-edit', branch], check=True, stdout=subprocess.DEVNULL)
    finish('Reviewed, committed and merged fixture feature')
elif 'E2E_FALSE_SUCCESS' in prompt:
    finish('Claimed complete without writing files')
else:
    Path('feature.txt').write_text('Fixture feature works\n')
    finish('Implemented fixture feature')
