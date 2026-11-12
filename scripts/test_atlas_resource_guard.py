"""Regression checks for the indexer's external resource supervisor."""
import importlib.util
import errno
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch
from pathlib import Path

spec = importlib.util.spec_from_file_location("atlas_resource_guard", Path(__file__).with_name("atlas_resource_guard.py"))
guard = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = guard
spec.loader.exec_module(guard)


def process_is_running(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    if sys.platform.startswith("linux"):
        try:
            state = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0]
        except FileNotFoundError:
            return False
        # A killed orphan may await reaping by a container's PID 1.
        return state != "Z"
    return True


class ResourceGuardTests(unittest.TestCase):
    def limits(self):
        return guard.Limits(memory_bytes=100, swap_growth_bytes=20, minimum_free_bytes=50,
                            read_bytes_per_second=10, read_window_seconds=5)

    def observation(self, **changes):
        values = dict(memory_bytes=90, swap_bytes=100, free_bytes=100, read_bytes=0)
        return guard.Observation(**(values | changes))

    def test_footprint_violation_trips_even_when_resident_memory_would_fit(self):
        monitor = guard.Monitor(self.limits(), initial_swap_bytes=100)
        self.assertEqual(monitor.check(self.observation(memory_bytes=101), now=0), "memory")
        self.assertIsNone(monitor.check(self.observation(memory_bytes=100), now=0))

    def test_disk_and_swap_are_independent_stop_conditions(self):
        monitor = guard.Monitor(self.limits(), initial_swap_bytes=100)
        self.assertEqual(monitor.check(self.observation(free_bytes=49), now=0), "storage")
        self.assertEqual(monitor.check(self.observation(swap_bytes=121), now=0), "swap")

    def test_reads_require_a_whole_window_and_counter_reset_is_safe(self):
        monitor = guard.Monitor(self.limits(), initial_swap_bytes=100)
        self.assertIsNone(monitor.check(self.observation(read_bytes=0), now=0))
        self.assertIsNone(monitor.check(self.observation(read_bytes=100), now=1))
        self.assertEqual(monitor.check(self.observation(read_bytes=150), now=5), "disk_reads")
        self.assertIsNone(monitor.check(self.observation(read_bytes=1), now=6))

    def test_old_read_burst_does_not_trip_forever(self):
        monitor = guard.Monitor(self.limits(), initial_swap_bytes=100)
        for now, count in [(0, 0), (1, 100), (4, 100), (6, 100)]:
            monitor.check(self.observation(read_bytes=count), now=now)
        self.assertIsNone(monitor.check(self.observation(read_bytes=100), now=11))

    def test_linux_zombie_is_distinguished_from_running_descendant(self):
        with patch.object(sys, "platform", "linux"), patch.object(os, "kill"):
            with patch.object(Path, "read_text", return_value="123 (worker name) Z 1 123"):
                self.assertFalse(process_is_running(123))
            with patch.object(Path, "read_text", return_value="123 (worker name) S 1 123"):
                self.assertTrue(process_is_running(123))

    def test_nonpositive_limits_are_rejected(self):
        with self.assertRaises(ValueError):
            guard.Limits(memory_bytes=0, swap_growth_bytes=20, minimum_free_bytes=50,
                         read_bytes_per_second=10, read_window_seconds=5)

    def test_linux_process_status_accepts_text_and_empty_fields(self):
        status = "Name:\tatlas\nState:\tS (sleeping)\nVmRSS:\t40 kB\nVmSwap:\t9 kB\n"
        def read_text(path):
            return status if str(path).endswith("/status") else "read_bytes: 123\nwrite_bytes: 0\n"
        with patch.object(guard.sys, "platform", "linux"), patch.object(Path, "read_text", read_text):
            self.assertEqual(guard.process_usage(123), (49 * 1024, 123))

    @unittest.skipUnless(sys.platform == "darwin", "Darwin zombie group signal semantics")
    def test_stop_reaps_a_zombie_only_owned_group_on_darwin(self):
        child = subprocess.Popen(["/usr/bin/true"], start_new_session=True)
        try:
            deadline = time.monotonic() + 5
            while True:
                state = subprocess.check_output(
                    ["ps", "-p", str(child.pid), "-o", "stat="], text=True).strip()
                if state.startswith("Z"):
                    break
                self.assertLess(time.monotonic(), deadline, "child did not exit")
                time.sleep(0.01)
            # Do not poll/reap first: XNU reports EPERM for this zombie-only group.
            guard.stop_child(child)
            self.assertEqual(child.returncode, 0)
        finally:
            child.wait()

    def test_zombie_group_probe_keeps_leader_unreaped_until_cleanup(self):
        from unittest.mock import Mock
        child = Mock(pid=123, returncode=None)
        child.poll.side_effect = AssertionError("leader cannot be reaped before the last group access")
        live = subprocess.CompletedProcess([], 0, stdout="123 123 Z\n124 123 S\n456 456 S\n")
        dead = subprocess.CompletedProcess([], 0, stdout="123 123 Z\n456 456 S\n")
        with patch.object(guard.sys, "platform", "darwin"), \
                patch.object(os, "killpg", side_effect=[None, PermissionError(errno.EPERM, "denied")]) as send, \
                patch.object(subprocess, "run", side_effect=[live, dead]):
            guard.stop_child(child)
            self.assertEqual([call.args[1] for call in send.call_args_list], [signal.SIGTERM, 0])
            child.wait.assert_called_once()
            child.poll.assert_not_called()

    def test_group_permission_failure_with_a_live_leader_is_not_ignored(self):
        from unittest.mock import Mock
        child = Mock(pid=123, returncode=None)
        table = subprocess.CompletedProcess([], 0, stdout="123 123 S\n")
        with patch.object(guard.sys, "platform", "darwin"), \
                patch.object(os, "killpg", side_effect=PermissionError(errno.EPERM, "denied")), \
                patch.object(subprocess, "run", return_value=table):
            with self.assertRaises(PermissionError):
                guard.stop_child(child)
            child.wait.assert_not_called()
            child.poll.assert_not_called()

    def test_group_permission_failure_with_a_live_descendant_is_not_ignored(self):
        from unittest.mock import Mock
        child = Mock(pid=123, returncode=None)
        table = subprocess.CompletedProcess([], 0, stdout="123 123 Z\n124 123 S\n456 456 Z\n")
        with patch.object(guard.sys, "platform", "darwin"), \
                patch.object(os, "killpg", side_effect=PermissionError(errno.EPERM, "denied")), \
                patch.object(subprocess, "run", return_value=table):
            with self.assertRaises(PermissionError):
                guard.stop_child(child)
            child.wait.assert_not_called()
            child.poll.assert_not_called()

    def test_group_permission_failure_with_unreadable_inventory_is_not_ignored(self):
        from unittest.mock import Mock
        child = Mock(pid=123, returncode=None)
        with patch.object(guard.sys, "platform", "darwin"), \
                patch.object(os, "killpg", side_effect=PermissionError(errno.EPERM, "denied")), \
                patch.object(subprocess, "run", side_effect=OSError("unavailable")):
            with self.assertRaises(PermissionError):
                guard.stop_child(child)
            child.wait.assert_not_called()
            child.poll.assert_not_called()

    def test_stop_escalates_for_a_child_ignoring_term(self):
        child = subprocess.Popen(
            [sys.executable, "-c",
             "import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print('ready',flush=True); time.sleep(60)"],
            start_new_session=True, stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(child.stdout.readline().strip(), "ready")
            guard.stop_child(child, grace_seconds=0.05)
            self.assertEqual(child.returncode, -signal.SIGKILL)
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            child.stdout.close()

    @unittest.skipUnless(sys.platform == "darwin" or sys.platform.startswith("linux"), "supported resource platform")
    def test_actual_supervisor_stops_child_and_receipt_blocks_restart(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = root / "stopped.json"
            started = root / "started"
            child_program = (
                "from pathlib import Path; import time; "
                f"Path({str(started)!r}).write_text('started'); "
                "allocation=bytearray(32*1024*1024); time.sleep(60)")
            command = [
                sys.executable, str(Path(guard.__file__)),
                "--max-memory-mib", "1", "--min-free-gib", "1",
                "--storage-path", directory, "--stop-file", str(receipt),
                "--", sys.executable, "-c", child_program]
            result = subprocess.run(command, capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)
            saved = json.loads(receipt.read_text())
            self.assertEqual(saved["reason"], "memory")
            with self.assertRaises(ProcessLookupError):
                os.kill(saved["pid"], 0)
            started.unlink(missing_ok=True)
            again = subprocess.run(command, capture_output=True, text=True, timeout=5)
            self.assertEqual(again.returncode, 0, again.stderr)
            self.assertFalse(started.exists(), "a stop receipt must prevent relaunch")
            self.assertEqual(saved, json.loads(receipt.read_text()))

    def test_stop_kills_descendants_with_exited_or_running_leader(self):
        for leader_exits in (False, True):
            with self.subTest(leader_exits=leader_exits):
                self.assert_stops_descendant(leader_exits)

    def assert_stops_descendant(self, leader_exits):
        grandchild = (
            "import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); "
            "print('ready',flush=True); time.sleep(60)")
        parent = (
            "import subprocess,sys,time; "
            f"p=subprocess.Popen([sys.executable,'-c',{grandchild!r}],stdout=subprocess.PIPE,text=True); "
            "p.stdout.readline(); print(p.pid,flush=True); " +
            ("sys.exit(0)" if leader_exits else "time.sleep(60)"))
        child = subprocess.Popen(
            [sys.executable, "-c", parent], start_new_session=True,
            stdout=subprocess.PIPE, text=True)
        descendant = int(child.stdout.readline())
        if leader_exits:
            deadline = time.monotonic() + 5
            while not guard.leader_has_exited(child):
                self.assertLess(time.monotonic(), deadline, "leader did not exit while held")
                time.sleep(0.01)
        try:
            guard.stop_child(child, grace_seconds=0.05)
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                if not process_is_running(descendant):
                    break
                time.sleep(0.01)
            else:
                self.fail("a descendant survived its supervised process group")
        finally:
            # Never signal the old numeric group after stop_child released its leader.
            if child.returncode is None:
                guard.stop_child(child, grace_seconds=0.05)
            child.stdout.close()

    def test_failed_startup_fence_prevents_any_child_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            fake_sampler = unittest.mock.Mock()
            fake_sampler.system_usage.return_value = (100, 1000)
            fake_sampler.sample.return_value = self.observation(memory_bytes=101)
            with patch.object(guard, "Sampler", return_value=fake_sampler), \
                    patch.object(guard, "write_stop_receipt", side_effect=OSError(errno.ENOSPC, "full")), \
                    patch.object(guard.subprocess, "Popen") as launch:
                for _ in range(2):
                    try:
                        guard.supervise([sys.executable, "-c", "import time;time.sleep(60)"],
                                        self.limits(), [Path(directory)], Path(directory) / "stop.json")
                    except OSError:
                        pass
                self.assertEqual(launch.call_count, 0, "a child needs a durable fence before starting")

    def test_failed_receipt_update_retains_durable_startup_fence(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "stop.json"
            fake_sampler = unittest.mock.Mock()
            fake_sampler.system_usage.return_value = (100, 1000)
            fake_sampler.sample.return_value = self.observation(memory_bytes=101)
            write = guard.write_stop_receipt
            calls = 0
            child = unittest.mock.Mock(pid=123, returncode=None)
            def stopped(owned):
                owned.returncode = -signal.SIGTERM
            def fail_update(*args, **kwargs):
                nonlocal calls
                calls += 1
                if calls == 1:
                    return write(*args, **kwargs)
                raise OSError(errno.ENOSPC, "full")
            with patch.object(guard, "Sampler", return_value=fake_sampler), \
                    patch.object(guard, "write_stop_receipt", side_effect=fail_update), \
                    patch.object(guard.subprocess, "Popen", return_value=child) as launch, \
                    patch.object(guard, "leader_has_exited", return_value=False), \
                    patch.object(guard.signal, "signal"), \
                    patch.object(guard, "stop_child", side_effect=stopped):
                for _ in range(2):
                    guard.supervise([sys.executable, "-c", "import time;time.sleep(60)"],
                                    self.limits(), [Path(directory)], marker)
                self.assertEqual(launch.call_count, 1)
                self.assertEqual(json.loads(marker.read_text())["reason"], "supervising")

    def test_stop_receipt_has_no_command_line_or_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "stop.json"
            guard.write_stop_receipt(marker, "memory", 123, self.observation(memory_bytes=101))
            text = marker.read_text()
            self.assertIn('"reason": "memory"', text)
            self.assertNotIn("environ", text)
            self.assertNotIn("command", text)
            self.assertEqual(marker.stat().st_mode & 0o777, 0o600)


class OwnedGroupLifetimeTests(unittest.TestCase):
    """Mock-only ownership regressions; run separately from the real-process cases."""

    class Child:
        pid = 123

        def __init__(self, events, *, exited=True, reaped=False):
            self.events = events
            self.exited = exited
            self.exit_status = 0
            self.returncode = self.exit_status if reaped else None

        def poll(self):
            self.events.append(("poll",))
            if self.exited:
                self.returncode = self.exit_status  # Popen.poll really relinquishes the PID pin.
            return self.returncode

        def wait(self, *args, **kwargs):
            self.events.append(("wait",))
            self.returncode = self.exit_status
            return self.returncode

    def setUp(self):
        def forbidden(*args, **kwargs):
            raise AssertionError("real process, signal, native, sampler or receipt effect forbidden")
        for obj, name in ((guard.ctypes, "CDLL"), (guard.subprocess, "Popen"),
                          (guard.subprocess, "run"), (guard.os, "kill"),
                          (guard.os, "killpg"), (guard.os, "waitpid"),
                          (guard.os, "waitid"), (guard.os, "wait"), (guard.os, "_exit"),
                          (guard.os, "pipe"), (guard.signal, "signal"),
                          (guard.signal, "pthread_sigmask"), (guard.signal, "sigpending"),
                          (guard.signal, "setitimer"),
                          (guard.time, "sleep"), (guard, "Sampler"),
                          (guard, "write_stop_receipt")):
            mocker = patch.object(obj, name, side_effect=forbidden)
            mocker.start()
            self.addCleanup(mocker.stop)

    def inventory(self, child, events, leader="Z", descendant=None):
        def read(command, **kwargs):
            self.assertIsNone(child.returncode, "process inventory used a released leader identity")
            self.assertEqual(command[0], "/bin/ps")
            self.assertLessEqual(kwargs.get("timeout", float("inf")), 2)
            events.append(("ps",))
            # Accept either the existing group-only view or a leader-aware view.
            # This models OS facts rather than requiring a particular new helper.
            columns = next((part for part in command if "stat=" in part), "")
            rows = []
            if "pid=" in columns.replace("pgid=", ""):
                rows.append(f"123 123 {leader}")
                if descendant is not None:
                    rows.append(f"124 123 {descendant}")
                rows.append("456 456 S")
            elif "pgid=" in columns:
                rows.append(f"123 {leader}")
                if descendant is not None:
                    rows.append(f"123 {descendant}")
                rows.append("456 S")
            else:
                rows.append(leader)
            if "-p" in command:
                rows = rows[:1]  # A real ps -p query returns only the requested leader.
            return subprocess.CompletedProcess(command, 0, stdout="\n".join(rows) + "\n")
        return read

    def test_cleanup_refuses_reaped_leader_even_if_numeric_group_exists(self):
        events = []
        child = self.Child(events, reaped=True)
        with patch.object(guard.os, "killpg") as send:
            with self.assertRaises(RuntimeError):
                guard.stop_child(child, grace_seconds=0)
        send.assert_not_called()
        self.assertEqual(events, [], "a released handle cannot recover group authority")

    def test_direct_group_signal_refuses_reaped_leader(self):
        child = self.Child([], reaped=True)
        with patch.object(guard.os, "killpg") as send:
            with self.assertRaises(RuntimeError):
                guard.signal_owned_group(child, signal.SIGKILL)
        send.assert_not_called()

    def test_live_descendants_are_drained_before_the_held_leader_is_reaped(self):
        for leader_exited in (False, True):
            with self.subTest(leader_exited=leader_exited):
                events = []
                child = self.Child(events, exited=leader_exited)
                killed = []
                def send(pid, signum):
                    self.assertEqual(pid, child.pid)
                    self.assertIsNone(child.returncode, "group operation after leader reap can hit reused PGID")
                    events.append(("killpg", signum))
                    if signum == signal.SIGKILL:
                        killed.append(True)
                        if not child.exited:
                            child.exited = True
                            child.exit_status = -signal.SIGKILL
                def read(command, **kwargs):
                    return self.inventory(child, events,
                                          leader="Z" if leader_exited or killed else "S",
                                          descendant="Z" if killed else "S")(command, **kwargs)
                with patch.object(guard.os, "killpg", side_effect=send), \
                        patch.object(guard.subprocess, "run", side_effect=read), \
                        patch.object(guard.time, "sleep"):
                    guard.stop_child(child, grace_seconds=0)
                self.assertTrue(killed, "necessary descendant cleanup was skipped")
                self.assertEqual(events.count(("wait",)), 1)
                self.assertEqual(events[-1], ("wait",), "wait must follow the last group access")
                self.assertNotIn(("poll",), events, "poll reaps before group drain is complete")

    def test_darwin_zombie_only_eperm_is_accepted_without_early_reap(self):
        events = []
        child = self.Child(events)
        def send(pid, signum):
            self.assertIsNone(child.returncode)
            events.append(("killpg", signum))
            raise PermissionError(errno.EPERM, "zombie-only XNU group")
        with patch.object(guard.sys, "platform", "darwin"), \
                patch.object(guard.os, "killpg", side_effect=send), \
                patch.object(guard.subprocess, "run", side_effect=self.inventory(child, events)):
            guard.stop_child(child)
        self.assertEqual(events.count(("wait",)), 1)
        self.assertEqual(events[-1], ("wait",))
        self.assertNotIn(("poll",), events)

    def test_eperm_with_live_or_unreadable_members_is_never_treated_as_drained(self):
        for kind in ("leader-live", "descendant-live", "unreadable", "malformed"):
            with self.subTest(kind=kind):
                events = []
                child = self.Child(events, exited=kind != "leader-live")
                if kind == "unreadable":
                    reader = OSError("inventory unavailable")
                elif kind == "malformed":
                    reader = subprocess.CompletedProcess([], 0, stdout="unparseable inventory\n")
                else:
                    reader = self.inventory(child, events,
                                            leader="S" if kind == "leader-live" else "Z",
                                            descendant="S" if kind == "descendant-live" else None)
                with patch.object(guard.sys, "platform", "darwin"), \
                        patch.object(guard.os, "killpg", side_effect=PermissionError(errno.EPERM, "denied")), \
                        patch.object(guard.subprocess, "run", side_effect=reader if callable(reader) or isinstance(reader, BaseException) else None,
                                     return_value=reader):
                    with self.assertRaises((PermissionError, RuntimeError)):
                        guard.stop_child(child)
                self.assertIsNone(child.returncode, "failed cleanup cannot release identity then retry")
                self.assertNotIn(("wait",), events)
                self.assertNotIn(("poll",), events)

    def test_missing_group_reaps_once_after_the_last_signal_attempt(self):
        events = []
        child = self.Child(events)
        def missing(pid, signum):
            self.assertIsNone(child.returncode)
            events.append(("killpg", signum))
            raise ProcessLookupError(errno.ESRCH, "gone")
        with patch.object(guard.os, "killpg", side_effect=missing):
            guard.stop_child(child)
        self.assertEqual(events, [("killpg", signal.SIGTERM), ("wait",)])

    def test_supervisor_keeps_exited_leader_pinned_through_final_cleanup(self):
        events = []
        child = self.Child(events)
        class StopFile:
            def exists(self):
                return False
            def unlink(self):
                events.append(("unlink",))
        sampler = unittest.mock.Mock()
        sampler.system_usage.return_value = (100, 1000)
        sampler.sample.return_value = guard.Observation(90, 100, 1000, 0)
        limits = guard.Limits(100, 20, 50, 10, 5)
        def send(pid, signum):
            self.assertIsNone(child.returncode, "supervisor reaped before its finally cleanup")
            events.append(("killpg", signum))
        with patch.object(guard, "Sampler", return_value=sampler), \
                patch.object(guard, "write_stop_receipt"), \
                patch.object(guard.subprocess, "Popen", return_value=child), \
                patch.object(guard.subprocess, "run", side_effect=self.inventory(child, events)), \
                patch.object(guard.os, "killpg", side_effect=send), \
                patch.object(guard.signal, "signal"), \
                patch.object(guard.time, "sleep"):
            self.assertEqual(guard.supervise(["fixed-no-execution"], limits, [], StopFile()), 0)
        self.assertNotIn(("poll",), events)
        self.assertEqual(events.count(("wait",)), 1)
        wait_index = events.index(("wait",))
        self.assertFalse(any(event[0] in ("killpg", "ps") for event in events[wait_index + 1:]))
        self.assertEqual(events[-1], ("unlink",), "natural successful exit should retain existing fence semantics")


class SigchldPolicyTests(unittest.TestCase):
    """Unsafe child-reaping policies must be refused before any child is spawned."""

    def assert_refuses_policy(self, disposition):
        def forbidden(*args, **kwargs):
            raise AssertionError("real native/process/signal boundary forbidden")
        # Nothing changes the real signal disposition; the getter is a pure fake.
        for obj, name in ((guard.ctypes, "CDLL"), (guard.subprocess, "run"),
                          (guard.os, "kill"), (guard.os, "killpg"),
                          (guard.os, "waitpid"), (guard.os, "waitid"),
                          (guard.os, "wait"), (guard.os, "_exit"), (guard.os, "pipe"),
                          (guard.signal, "pthread_sigmask"), (guard.signal, "sigpending"),
                          (guard.signal, "setitimer"), (guard.time, "sleep")):
            mocker = patch.object(obj, name, side_effect=forbidden)
            mocker.start()
            self.addCleanup(mocker.stop)
        class StopFile:
            def exists(self):
                return False
        sampler = unittest.mock.Mock()
        sampler.system_usage.return_value = (100, 1000)
        with patch.object(guard.signal, "getsignal", return_value=disposition) as getter, \
                patch.object(guard.signal, "signal"), \
                patch.object(guard, "Sampler", return_value=sampler), \
                patch.object(guard, "write_stop_receipt"), \
                patch.object(guard.subprocess, "Popen", side_effect=RuntimeError("mock intercepted unsafe spawn")) as launch:
            with self.assertRaises(RuntimeError):
                guard.supervise(["never-executed"], guard.Limits(100, 20, 50, 10, 5), [], StopFile())
            launch.assert_not_called()
            getter.assert_called_once_with(signal.SIGCHLD)

    def test_ignored_sigchld_refuses_child_spawn(self):
        self.assert_refuses_policy(signal.SIG_IGN)

    def test_custom_reaper_refuses_child_spawn(self):
        def reaper(*args):
            raise AssertionError("the fake reaping handler must never be invoked")
        self.assert_refuses_policy(reaper)


if __name__ == "__main__":
    unittest.main()
