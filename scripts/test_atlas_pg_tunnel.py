"""Owned local subprocess checks; these tests never run SSH."""
import importlib.util
import argparse
import json
import os
from pathlib import Path
import socket
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

spec=importlib.util.spec_from_file_location("atlas_pg_tunnel",Path(__file__).with_name("atlas_pg_tunnel.py"))
tunnel=importlib.util.module_from_spec(spec)
sys.modules[spec.name]=tunnel
spec.loader.exec_module(tunnel)

class TunnelTests(unittest.TestCase):
    def test_transient_exit_retries_its_child_and_stops(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            record=root/"attempts"
            script=root/"forward.py"
            script.write_text("import pathlib,socket,sys,time\np=pathlib.Path(sys.argv[1]);n=int(p.read_text())+1 if p.exists() else 1;p.write_text(str(n))\nif n==1: sys.stderr.write('Connection refused\\n');sys.exit(255)\ns=socket.socket(socket.AF_UNIX);s.bind(sys.argv[2]);s.listen(1)\nwhile True: time.sleep(.05)\n")
            stop=threading.Event()
            def finish():
                deadline=time.monotonic()+5
                while time.monotonic()<deadline:
                    if record.exists() and record.read_text()=="2" and (root/"pg.sock").exists():break
                    time.sleep(.01)
                stop.set()
            watcher=threading.Thread(target=finish);watcher.start()
            try:
                result=tunnel.supervise([sys.executable,str(script),str(record),str(root/"pg.sock")],root/"pg.sock",root/"stop.json",stop,initial_backoff=.02,max_backoff=.05)
                self.assertEqual(result,0)
                self.assertTrue(record.exists(),"supervisor never started its owned child")
                self.assertEqual(record.read_text(),"2")
                self.assertFalse((root/"pg.sock").exists())
            finally:
                stop.set();watcher.join()


class TunnelSafetyTests(unittest.TestCase):
    def run_configured_exit(self, diagnostic, *, fatal=False):
        """Model OpenSSH log severity at the real command/supervisor boundary.

        Only the SSH executable is replaced. No SSH connection is made; the
        configured -o LogLevel controls whether the fixture emits its INFO
        keepalive diagnostic. Authentication/configuration errors are ERROR.
        """
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            key = root / "identity"
            key.write_text("test fixture")
            key.chmod(0o600)
            known = root / "known_hosts"
            known.write_text("")
            args = argparse.Namespace(host="ssh.example.invalid", user="example", port=2222,
                remote_host="127.0.0.1", remote_port=5432, socket=root / "pg.sock",
                identity_file=key, known_hosts_file=known)
            command = tunnel.build_command(args)
            fixture = root / "ssh_fixture.py"
            fixture.write_text("""import json,os,pathlib,socket,sys,time
root=pathlib.Path(sys.argv[1])
diagnostic=json.loads(sys.argv[2]);fatal=sys.argv[3]=='True'
options=[sys.argv[i+1] for i,value in enumerate(sys.argv[:-1]) if value=='-o']
levels={'QUIET':0,'FATAL':1,'ERROR':2,'INFO':3,'VERBOSE':4,'DEBUG':5,'DEBUG1':5,'DEBUG2':6,'DEBUG3':7}
level=next((value.split('=',1)[1].upper() for value in options if value.lower().startswith('loglevel=')),'INFO')
p=root/'attempts';n=int(p.read_text())+1 if p.exists() else 1
temporary=root/'attempts.tmp';temporary.write_text(str(n));temporary.replace(p)
s=socket.socket(socket.AF_UNIX);s.bind(str(root/'pg.sock'));s.listen(1)
(root/f'ready-{os.getpid()}').write_text('ready')
release=root/f'release-{os.getpid()}';deadline=time.monotonic()+2
while not release.exists():
    if time.monotonic()>=deadline:
        (root/'handshake_failed').write_text('child was not released after measurement')
        raise RuntimeError('fixture measurement handshake timed out')
    time.sleep(.005)
if fatal or n<3:
    if levels[level]>=(2 if fatal else 3):
        sys.stderr.write(diagnostic+'\\n');sys.stderr.flush()
    sys.exit(255)
while True: time.sleep(.01)
""")
            command[:1] = [sys.executable, str(fixture), str(root), json.dumps(diagnostic), str(fatal)]
            stop = threading.Event()
            timer = threading.Timer(5, stop.set)
            timer.start()
            real_popen = tunnel.subprocess.Popen
            def start_ready(*command_args, **command_options):
                child = real_popen(*command_args, **command_options)
                try:
                    deadline = time.monotonic() + 2
                    ready = root / f"ready-{child.pid}"
                    while not ready.exists():
                        if child.poll() is not None or time.monotonic() >= deadline:
                            raise RuntimeError("fixture child readiness timed out")
                        time.sleep(.005)
                    return child
                except BaseException:
                    tunnel.stop_child(child, grace=.1)
                    child.stderr.close()
                    raise
            def finish(pid, memory, tail):
                # supervise calls the observer only after the real sample
                # succeeds; release only that measured, ready child to exit.
                (root / f"release-{pid}").write_text("measured")
                record = root / "attempts"
                if record.exists() and record.read_text() == "3" and args.socket.exists():
                    stop.set()
            try:
                # These classification fixtures take one real sample per child.
                # The five-second overall stop precedes a second ten-second
                # sample, keeping deliberate fixture teardown out of /proc reads.
                # Wait at the real launch boundary so lease.capture also sees
                # the ready socket before the observer lets its child exit.
                with patch.object(tunnel.subprocess, "Popen", start_ready):
                    result = tunnel.supervise(command, args.socket, root / "stop.json", stop,
                        initial_backoff=.02, max_backoff=.05, sample_seconds=10, observer=finish)
                self.assertFalse((root / "handshake_failed").exists(), "fixture measurement handshake failed")
                receipt = (root / "stop.json").read_text() if (root / "stop.json").exists() else None
                return result, receipt, int((root / "attempts").read_text()), args.socket.exists()
            finally:
                stop.set()
                timer.cancel()
                timer.join()

    def test_configured_logging_retries_established_keepalive_timeouts(self):
        result, receipt, attempts, remaining_socket = self.run_configured_exit(
            "Timeout, server ssh.example.invalid not responding.")
        self.assertEqual(result, 0, receipt)
        self.assertIsNone(receipt)
        self.assertEqual(attempts, 3, "keepalive loss must retry before recovery")
        self.assertFalse(remaining_socket)

    def test_configured_logging_preserves_fatal_diagnostic_precedence(self):
        for diagnostic in ("Permission denied (publickey).", "Host key verification failed.",
                           "Bad configuration option: invalid", "bind: Address already in use",
                           "unclassified failure"):
            with self.subTest(diagnostic=diagnostic):
                # Recognized transport text must never overrule fatal evidence;
                # an unclassified 255 remains fatal without that extra text.
                message = diagnostic
                if diagnostic != "unclassified failure":
                    message += "\nTimeout, server ssh.example.invalid not responding."
                result, receipt, attempts, remaining_socket = self.run_configured_exit(message, fatal=True)
                self.assertEqual(result, 1)
                self.assertIn("ssh_failed", receipt)
                self.assertEqual(attempts, 1)
                self.assertFalse(remaining_socket)

    def test_initial_unknown_socket_and_file_are_preserved(self):
        for kind in ("file", "socket", "symlink"):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                path = root / "pg.sock"
                listener = None
                if kind == "file":
                    path.write_text("unowned")
                elif kind == "symlink":
                    path.symlink_to(root / "missing")
                else:
                    listener = socket.socket(socket.AF_UNIX)
                    listener.bind(str(path))
                    listener.listen(1)
                before = path.lstat().st_ino
                try:
                    with self.assertRaises(tunnel.Refused):
                        tunnel.SocketLease(path)
                    self.assertEqual(path.lstat().st_ino, before)
                finally:
                    if listener is not None:
                        listener.close()

    def test_replaced_inode_and_live_orphan_are_not_removed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "pg.sock"
            lease = tunnel.SocketLease(path)
            original = socket.socket(socket.AF_UNIX)
            original.bind(str(path))
            original.listen(1)
            lease.capture()
            with self.assertRaises(tunnel.Refused):
                lease.cleanup()
            self.assertTrue(path.exists())
            original.close()
            path.rename(root / "old.sock")
            path.write_text("replacement")
            with self.assertRaises(tunnel.Refused):
                lease.cleanup()
            self.assertEqual(path.read_text(), "replacement")

    def run_fake(self, body, *, action=None, **options):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "pg.sock"
            script = root / "forward.py"
            script.write_text("import os,pathlib,signal,socket,sys,time\nroot=pathlib.Path(sys.argv[1])\n" + body)
            stop = threading.Event()
            state = {"root": root, "stop": stop, "pids": [], "memory": [], "tails": [], "fds": []}
            def observe(pid, memory, tail):
                try:
                    group = os.getpgid(pid)
                except ProcessLookupError:
                    return  # Exact sampled child exited before this read-only probe.
                self.assertEqual(group, os.getpgrp(), "child escaped guarded group")
                state["pids"].append(pid)
                state["memory"].append(memory)
                state["tails"].append(tail)
                directory = "/dev/fd" if sys.platform == "darwin" else "/proc/self/fd"
                state["fds"].append(len(os.listdir(directory)))
                if action is not None:
                    action(state)
            timer = threading.Timer(5, stop.set)
            timer.start()
            try:
                result = tunnel.supervise(
                    [sys.executable, str(script), str(root)], path, root / "stop.json", stop,
                    initial_backoff=.02, max_backoff=.05, sample_seconds=.01,
                    observer=observe, **options)
                receipt = (root / "stop.json").read_text() if (root / "stop.json").exists() else None
                state["remaining_file"] = path.read_text() if path.is_file() else None
                for pid in set(state["pids"]):
                    with self.assertRaises(ProcessLookupError):
                        os.kill(pid, 0)
                return result, receipt, state
            finally:
                stop.set()
                timer.cancel()
                timer.join()

    def test_repeated_transport_loss_reaps_children_and_bounds_diagnostics(self):
        def finish(state):
            record = state["root"] / "attempts"
            if record.exists() and int(record.read_text()) >= 5 and (state["root"] / "pg.sock").exists():
                state["stop"].set()
        body = """p=root/'attempts'
n=int(p.read_text())+1 if p.exists() else 1
temporary=root/'attempts.tmp';temporary.write_text(str(n));temporary.replace(p)
s=socket.socket(socket.AF_UNIX);s.bind(str(root/'pg.sock'));s.listen(1)
sys.stderr.write('x'*8000);sys.stderr.flush()
time.sleep(.08)
sys.stderr.write('Connection reset by peer\\n');sys.stderr.flush()
sys.exit(255)
"""
        result, receipt, state = self.run_fake(body, action=finish)
        self.assertEqual(result, 0, receipt)
        self.assertIsNone(receipt)
        self.assertGreaterEqual(len(set(state["pids"])), 5)
        self.assertLessEqual(max(state["tails"]), tunnel.TAIL_BYTES)
        self.assertLess(max(state["memory"]), tunnel.MAX_MEMORY)
        self.assertLessEqual(max(state["fds"]), min(state["fds"]) + 2)

    def test_resource_and_measurement_failures_are_fenced_without_retry(self):
        body = "s=socket.socket(socket.AF_UNIX);s.bind(str(root/'pg.sock'));s.listen(1)\nwhile True: time.sleep(.01)\n"
        for measurement, reason in [(lambda *_: (tunnel.MAX_MEMORY + 1, 0), 'memory'),
                                    (lambda *_: (_ for _ in ()).throw(OSError('unavailable')), 'measurement_failed')]:
            with patch.object(tunnel.guard, 'process_usage', measurement):
                result, receipt, state = self.run_fake(body)
            self.assertEqual(result, 1)
            self.assertIn(reason, receipt)
            self.assertLessEqual(len(set(state["pids"])), 1)

    def test_term_resistant_child_is_killed_and_reaped(self):
        body = "signal.signal(signal.SIGTERM,signal.SIG_IGN)\ns=socket.socket(socket.AF_UNIX);s.bind(str(root/'pg.sock'));s.listen(1)\nwhile True: time.sleep(.01)\n"
        def finish(state):
            if (state["root"] / "pg.sock").exists():
                state["stop"].set()
        began = time.monotonic()
        result, receipt, state = self.run_fake(body, action=finish, grace=.1)
        self.assertEqual(result, 0, receipt)
        self.assertIsNone(receipt)
        self.assertLess(time.monotonic() - began, 2)
        self.assertEqual(len(set(state["pids"])), 1)

    def test_fatal_authentication_does_not_become_a_network_retry(self):
        for message in ('Permission denied', 'Host key verification failed', 'unknown option', 'unclassified failure'):
            body = 'sys.stderr.write(' + repr(message + '\nConnection closed\n') + ');sys.exit(255)\n'
            # Unknown followed by an actual close is transport-classified, so test
            # unknown in isolation; known fatal evidence always takes precedence.
            if message == 'unclassified failure':
                body = "sys.stderr.write('unclassified failure\\n');sys.exit(255)\n"
            result, receipt, state = self.run_fake(body)
            self.assertEqual(result, 1)
            self.assertIn('ssh_failed', receipt)
            self.assertLessEqual(len(set(state["pids"])), 1)
        tail = tunnel.Diagnostics()
        tail.add(b'Permission denied\n')
        tail.add(b'x' * 40000)
        tail.add(b'Connection closed\n')
        self.assertFalse(tunnel.transient_exit(255, tail))
        self.assertEqual(len(tail), tunnel.TAIL_BYTES)

    def test_explicit_configuration_disables_uncontrolled_ssh_features(self):
        import argparse
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            key = root / 'identity'
            key.write_text('test fixture')
            key.chmod(0o600)
            known = root / 'known_hosts'
            known.write_text('')
            args = argparse.Namespace(host='ssh.example.invalid', user='example', port=2222,
                remote_host='127.0.0.1', remote_port=5432, socket=root/'pg.sock',
                identity_file=key, known_hosts_file=known)
            command = tunnel.build_command(args)
            self.assertEqual(command[:3], ['/usr/bin/ssh', '-F', '/dev/null'])
            self.assertIn('StrictHostKeyChecking=yes', command)
            self.assertIn('BatchMode=yes', command)
            for value in ['ControlMaster=no', 'ControlPath=none', 'ControlPersist=no',
                          'ForkAfterAuthentication=no', 'ProxyCommand=none', 'ProxyJump=none',
                          'PermitLocalCommand=no', 'StreamLocalBindUnlink=no']:
                self.assertIn(value, command)
            self.assertEqual(command[-2:], ['--', args.host])
            self.assertIn('example', command)
            self.assertIn('2222', command)
            args.host = '-bad'
            with self.assertRaises(ValueError):
                tunnel.build_command(args)

    def test_runtime_socket_replacement_is_retained_and_stops_retry(self):
        def replace(state):
            path = state["root"] / "pg.sock"
            if path.exists() and not state.get("replaced"):
                path.rename(state["root"] / "original.sock")
                path.write_text("unowned replacement")
                state["replaced"] = True
        body = "s=socket.socket(socket.AF_UNIX);s.bind(str(root/'pg.sock'));s.listen(1)\nwhile True: time.sleep(.01)\n"
        result, receipt, state = self.run_fake(body, action=replace)
        self.assertEqual(result, 1)
        self.assertIsNotNone(receipt)
        self.assertEqual(state["remaining_file"], "unowned replacement")
        self.assertEqual(len(set(state["pids"])), 1)

    def test_retained_fence_refuses_a_new_child(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = root / "stop.json"
            receipt.write_text("retained diagnostic")
            command = [sys.executable, "-c", "raise RuntimeError('must not run')"]
            self.assertEqual(tunnel.supervise(command, root/"pg.sock", receipt, threading.Event()), 1)
            self.assertEqual(receipt.read_text(), "retained diagnostic")
            self.assertFalse((root/"pg.sock").exists())

    def test_operator_cancellation_interrupts_maximum_backoff(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stop = threading.Event()
            timer = threading.Timer(.2, stop.set)
            timer.start()
            began = time.monotonic()
            try:
                command = [sys.executable, "-c", "import sys;sys.stderr.write('Connection refused\\n');sys.exit(255)"]
                result = tunnel.supervise(command, root/"pg.sock", root/"stop.json", stop,
                    initial_backoff=30, max_backoff=30)
                self.assertEqual(result, 0)
                self.assertLess(time.monotonic()-began, 1)
            finally:
                timer.cancel()
                timer.join()

    def test_cli_term_stops_and_reaps_exact_owned_child(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake = root/"forward.py"
            fake.write_text("import os,pathlib,socket,sys,time\nroot=pathlib.Path(sys.argv[1]);(root/'pid').write_text(str(os.getpid()))\ns=socket.socket(socket.AF_UNIX);s.bind(str(root/'pg.sock'));s.listen(1)\nwhile True:time.sleep(.01)\n")
            launch = root/"helper.py"
            launch.write_text("import importlib.util,sys\nfrom pathlib import Path\np=Path(sys.argv[1]);spec=importlib.util.spec_from_file_location('tunnel',sys.argv[2]);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)\nm.build_command=lambda _: [sys.executable,str(p/'forward.py'),str(p)]\nsys.argv=['helper','--host','ssh.example.invalid','--user','example','--identity-file',str(p/'key'),'--known-hosts-file',str(p/'known'),'--remote-host','127.0.0.1','--socket',str(p/'pg.sock'),'--stop-file',str(p/'stop.json')]\nraise SystemExit(m.main())\n")
            child = subprocess.Popen([sys.executable,str(launch),str(root),str(Path(tunnel.__file__).resolve())],
                stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,close_fds=True)
            try:
                deadline=time.monotonic()+3
                while not (root/"pg.sock").exists():
                    self.assertIsNone(child.poll())
                    self.assertLess(time.monotonic(),deadline)
                    time.sleep(.01)
                child.send_signal(signal.SIGTERM)
                self.assertEqual(child.wait(timeout=4),0)
                with self.assertRaises(ProcessLookupError):
                    os.kill(int((root/"pid").read_text()),0)
                self.assertFalse((root/"pg.sock").exists())
                self.assertFalse((root/"stop.json").exists())
            finally:
                if child.poll() is None:
                    child.kill()
                child.wait()

if __name__ == "__main__":
    unittest.main()
