"""Black-box daemon/IPC/provider-process/Git tests with disposable state."""
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
BINARY = ROOT / 'target/debug/prodex'

class Workflows(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdx-e2e-', dir='/tmp')
        self.root = Path(self.tmp.name).resolve()
        self.state = self.root / 'state'
        self.project = self.root / 'project with spaces'
        self.project.mkdir()
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        for provider in ('codex', 'claude'):
            shutil.copy(ROOT / 'tests/e2e/provider.py', self.bin / provider)
            (self.bin / provider).chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ['PATH'],
                        ANTHROPIC_API_KEY='fixture-not-a-real-key', E2E_FAILURE_MARKER=str(self.root / 'failed'),
                        GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_SYSTEM='/dev/null',
                        GIT_AUTHOR_NAME='E2E', GIT_AUTHOR_EMAIL='e2e@example.invalid',
                        GIT_COMMITTER_NAME='E2E', GIT_COMMITTER_EMAIL='e2e@example.invalid')
        self.git('init', '-b', 'main')
        (self.project / 'README.md').write_text('Fixture application\n')
        self.git('add', 'README.md')
        self.git('commit', '-m', 'Initial fixture')
        self.log = open(self.root / 'daemon.log', 'w+')
        self.daemon = None
        self.start()
        self.call('project', path=str(self.project), objective='Implement fixture feature', enabled=True)
        settings = self.call('status')['settings']
        settings.update(preferred_provider='codex', planner_provider='codex', max_concurrent=1, max_per_project=1, task_timeout_secs=15)
        self.call('configure', settings=settings)
    def tearDown(self):
        if self.daemon and self.daemon.poll() is None:
            try:
                self.call('shutdown')
                self.daemon.wait(timeout=10)
            except Exception:
                self.daemon.kill()
                self.daemon.wait()
        self.log.close()
        self.tmp.cleanup()
    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.project), *args], env=self.env, stderr=subprocess.STDOUT, text=True).strip()
    def start(self):
        self.daemon = subprocess.Popen([str(BINARY), '--state-dir', str(self.state), 'daemon'], env=self.env, stdout=self.log, stderr=self.log)
        self.wait(lambda: self.call('status'), 'daemon startup')
    def call(self, command, expect_ok=True, **data):
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(10)
            connection.connect(str(self.state / 'control.sock'))
            connection.sendall(json.dumps(dict(command=command, **data)).encode() + b'\n')
            with connection.makefile('rb') as stream:
                response = json.loads(stream.readline())
        if expect_ok:
            self.assertTrue(response['ok'], response)
            return response['data']
        self.assertFalse(response['ok'], response)
        return response['error']
    def wait(self, fn, description):
        deadline = time.monotonic() + 20
        last = None
        while time.monotonic() < deadline:
            try:
                last = fn()
                if last:
                    return last
            except (FileNotFoundError, ConnectionRefusedError):
                pass
            time.sleep(.05)
        self.log.flush()
        self.fail(f'{description} timed out: {last}; log={Path(self.log.name).read_text()}')
    def task(self, id):
        return next(t for t in self.call('status')['tasks'] if t['id'] == id)
    def status(self, id, status):
        return self.wait(lambda: (t if (t := self.task(id))['status'] == status else None), f'{id} -> {status}')
    def submit(self, provider='codex', prompt='E2E_FEATURE', approved=False, dependencies=None):
        return self.call('submit', approved=approved, proposal=dict(project=str(self.project), objective_version=1,
            prompt=prompt, rationale='Fixture feature acceptance', provider=provider, mode='edit', risk='low',
            expected_files=['feature.txt'], dependencies=dependencies or [], completion_criteria='Feature exists and is integrated'))
    def test_codex_and_claude_approval_coding_merge_and_restart(self):
        for provider in ('codex', 'claude'):
            with self.subTest(provider=provider):
                task = self.submit(provider, prompt=f'E2E_FEATURE_{provider}')
                time.sleep(.4)
                self.assertEqual(self.task(task['id'])['status'], 'awaiting_approval')
                self.call('approve', id=task['id'])
                done = self.status(task['id'], 'succeeded')
                self.assertEqual(done['review'], 'awaiting_review')
                self.assertIsNotNone(done['session_id'])
                self.assertTrue(Path(done['worktree'], 'feature.txt').exists())
                if provider == 'codex':
                    self.assertFalse((self.project / 'feature.txt').exists())
                else:
                    # Create a different change for the second independent provider.
                    Path(done['worktree'], 'feature.txt').write_text('Claude fixture feature\n')
                (self.project / 'unrelated.txt').write_text('Keep unrelated local file\n')
                job = self.call('merge', id=task['id'])
                self.status(job['id'], 'succeeded')
                self.assertEqual(self.task(task['id'])['review'], 'integrated')
                self.assertEqual((self.project / 'unrelated.txt').read_text(), 'Keep unrelated local file\n')
                self.assertEqual(self.git('status', '--porcelain'), '?? unrelated.txt')
        self.call('shutdown'); self.daemon.wait(timeout=10)
        self.start()
        self.assertEqual(sum(t['review'] == 'integrated' for t in self.call('status')['tasks']), 2)
    def test_failure_retry_and_dismiss_preserve_result(self):
        task = self.submit(prompt='E2E_FAIL_ONCE', approved=True)
        self.status(task['id'], 'needs_retry')
        self.call('retry', id=task['id'])
        done = self.status(task['id'], 'succeeded')
        self.assertEqual(len(done['attempts']), 1)
        self.call('reject', id=task['id'])
        self.assertEqual(self.task(task['id'])['status'], 'rejected')
        self.assertTrue(Path(done['worktree'], 'feature.txt').exists())
    def test_blocked_merge_manual_integration_then_retry_verifies(self):
        task = self.submit(approved=True)
        done = self.status(task['id'], 'succeeded')
        (self.project / 'BLOCK_MERGE').write_text('block')
        job = self.call('merge', id=task['id'])
        blocked = self.status(job['id'], 'needs_retry')
        self.assertIn('overlapping main edits', blocked['summary'])
        self.assertEqual(self.task(task['id'])['review'], 'awaiting_review')
        worktree = done['worktree']
        for args in [('add', 'feature.txt'), ('commit', '-m', 'Manual integration')]:
            subprocess.run(['git', '-C', worktree, *args], env=self.env, check=True, stdout=subprocess.DEVNULL)
        self.git('merge', '--no-edit', done['branch'])
        retry = self.call('merge', id=task['id'])
        self.wait(lambda: self.task(task['id'])['review'] == 'integrated', 'manual merge verified')
        self.assertEqual(self.task(task['id'])['review'], 'integrated')
        self.assertTrue((self.project / 'BLOCK_MERGE').exists())
    def test_pause_stop_and_unconnect(self):
        self.call('pause')
        task = self.submit(prompt='E2E_SLOW', approved=True)
        time.sleep(.4)
        self.assertEqual(self.task(task['id'])['status'], 'queued')
        self.call('resume')
        self.status(task['id'], 'running')
        self.call('stop', id=task['id'])
        self.status(task['id'], 'interrupted')
        self.call('unconnect_project', project=str(self.project))
        self.assertFalse(self.call('status')['projects'][0]['enabled'])
        self.assertTrue((self.project / 'README.md').exists())
    def test_main_folder_coding_and_review(self):
        self.call('set_worktrees', project=str(self.project), enabled=False)
        task = self.submit('claude', approved=True)
        done = self.status(task['id'], 'succeeded')
        self.assertIsNone(done['worktree'])
        self.assertTrue((self.project / 'feature.txt').exists())
        self.call('mark_reviewed', id=task['id'])
        self.assertEqual(self.task(task['id'])['review'], 'integrated')
    def test_planning_ranked_reserve_persists_and_requires_approval(self):
        self.call('plan', project=str(self.project))
        tasks = self.wait(lambda: (tasks if len(tasks := self.call('status')['tasks']) == 10 else None), 'ten ranked proposals')
        self.assertEqual([t['proposal']['prompt'] for t in tasks], [f'E2E_RANKED_{i}' for i in range(10)])
        self.assertTrue(all(t['status'] == 'awaiting_approval' for t in tasks))
        self.call('reject', id=tasks[0]['id'])
        pending = [t for t in self.call('status')['tasks'] if t['status'] == 'awaiting_approval']
        self.assertEqual(pending[0]['proposal']['prompt'], 'E2E_RANKED_1')
        self.assertFalse((self.project / 'feature.txt').exists())

    def test_timeout_and_invalid_provider_output_require_retry(self):
        settings = self.call('status')['settings']
        settings['task_timeout_secs'] = 1
        self.call('configure', settings=settings)
        task = self.submit(prompt='E2E_SLOW_TIMEOUT', approved=True)
        failed = self.status(task['id'], 'needs_retry')
        self.assertTrue(failed['summary'])
        malformed = self.submit(prompt='E2E_MALFORMED', approved=True)
        failed = self.status(malformed['id'], 'needs_retry')
        self.assertIn('Malformed', failed['summary'])

    def test_daemon_crash_requires_explicit_recovery(self):
        task = self.submit(prompt='E2E_SLOW_CRASH', approved=True)
        running = self.status(task['id'], 'running')
        # Kill only our disposable daemon; never signal a PID recovered from disk.
        self.daemon.kill()
        self.daemon.wait(timeout=10)
        self.start()
        recovered = self.status(task['id'], 'recovery_required')
        self.assertEqual(recovered['worktree'], running['worktree'])
        self.assertTrue(self.call('status')['settings']['paused'])
        self.call('retry', id=task['id'], expect_ok=False)
        # The fixture may survive daemon death. Reap only the process group
        # whose PID was captured directly from this test's live daemon.
        if running['pid']:
            import signal
            try:
                os.killpg(running['pid'], signal.SIGTERM)
            except ProcessLookupError:
                pass
        self.call('resolve_recovery', id=task['id'])
        self.assertEqual(self.task(task['id'])['status'], 'interrupted')

    def test_approval_rejection_and_duplicate_protection(self):
        task = self.submit()
        self.call('approve', id='does-not-exist', expect_ok=False)
        self.call('submit', expect_ok=False, approved=False, proposal=task['proposal'])
        self.call('reject', id=task['id'])
        self.call('approve', id=task['id'], expect_ok=False)
        self.assertEqual(self.task(task['id'])['status'], 'rejected')
        self.assertFalse((self.project / 'feature.txt').exists())

    def test_dependencies_wait_for_integration(self):
        parent = self.submit(approved=True)
        self.status(parent['id'], 'succeeded')
        child = self.submit(prompt='E2E_SLOW', approved=True, dependencies=[parent['id']])
        time.sleep(.4)
        self.assertEqual(self.task(child['id'])['status'], 'queued')
        job = self.call('merge', id=parent['id'])
        self.status(job['id'], 'succeeded')
        self.status(child['id'], 'running')
        self.call('stop', id=child['id'])
        self.status(child['id'], 'interrupted')

if __name__ == '__main__':
    unittest.main(verbosity=2)
