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
            child.wait(timeout=5)
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
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait()
            child.stdout.close()

    def test_failed_startup_fence_prevents_any_child_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            fake_sampler = unittest.mock.Mock()
            fake_sampler.system_usage.return_value = (100, 1000)
            fake_sampler.sample.return_value = self.observation(memory_bytes=101)
            with patch.object(guard, "Sampler", return_value=fake_sampler), \
                    patch.object(guard, "write_stop_receipt", side_effect=OSError(errno.ENOSPC, "full")), \
                    patch.object(guard.subprocess, "Popen", wraps=subprocess.Popen) as launch:
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
            def fail_update(*args, **kwargs):
                nonlocal calls
                calls += 1
                if calls == 1:
                    return write(*args, **kwargs)
                raise OSError(errno.ENOSPC, "full")
            with patch.object(guard, "Sampler", return_value=fake_sampler), \
                    patch.object(guard, "write_stop_receipt", side_effect=fail_update), \
                    patch.object(guard.subprocess, "Popen", wraps=subprocess.Popen) as launch:
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


if __name__ == "__main__":
    unittest.main()
