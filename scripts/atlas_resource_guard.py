#!/usr/bin/env python3
"""Supervise an Atlas indexer with memory, swap, disk-read and free-space limits.

The child runs in its own process group. A limit violation or failed measurement
stops that group and writes a private receipt. An existing receipt blocks a new
launch until the operator has reviewed the failure and explicitly removed it.
"""
import argparse
import collections
import ctypes
from dataclasses import asdict, dataclass
import json
import math
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import threading
import time

MIB = 1 << 20
GIB = 1 << 30


@dataclass(frozen=True)
class Limits:
    memory_bytes: int
    swap_growth_bytes: int
    minimum_free_bytes: int
    read_bytes_per_second: int
    read_window_seconds: float

    def __post_init__(self):
        if any(not math.isfinite(value) or value <= 0 for value in asdict(self).values()):
            raise ValueError("all resource limits must be finite and positive")


@dataclass(frozen=True)
class Observation:
    memory_bytes: int
    swap_bytes: int
    free_bytes: int
    read_bytes: int


class Monitor:
    def __init__(self, limits, initial_swap_bytes):
        self.limits = limits
        self.initial_swap_bytes = initial_swap_bytes
        self.reads = collections.deque()

    def check(self, observation, now):
        limits = self.limits
        if observation.memory_bytes > limits.memory_bytes:
            return "memory"
        if observation.free_bytes < limits.minimum_free_bytes:
            return "storage"
        if observation.swap_bytes - self.initial_swap_bytes > limits.swap_growth_bytes:
            return "swap"
        if self.reads and (now < self.reads[-1][0] or observation.read_bytes < self.reads[-1][1]):
            self.reads.clear()
        self.reads.append((now, observation.read_bytes))
        cutoff = now - limits.read_window_seconds
        while len(self.reads) > 1 and self.reads[1][0] <= cutoff:
            self.reads.popleft()
        first_time, first_bytes = self.reads[0]
        elapsed = now - first_time
        if elapsed >= limits.read_window_seconds:
            if (observation.read_bytes - first_bytes) / elapsed > limits.read_bytes_per_second:
                return "disk_reads"
        return None


class MacRUsageV2(ctypes.Structure):
    # Darwin sys/resource.h, rusage_info_v2. All fields after ri_uuid are u64.
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [
        (name, ctypes.c_uint64) for name in (
            "user_time", "system_time", "pkg_idle_wkups", "interrupt_wkups",
            "pageins", "wired_size", "resident_size", "phys_footprint",
            "proc_start_abstime", "proc_exit_abstime", "child_user_time",
            "child_system_time", "child_pkg_idle_wkups", "child_interrupt_wkups",
            "child_pageins", "child_elapsed_abstime", "diskio_bytesread",
            "diskio_byteswritten")]


def process_usage(pid):
    if sys.platform == "darwin":
        library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        call = library.proc_pid_rusage
        call.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.POINTER(MacRUsageV2)]
        call.restype = ctypes.c_int
        usage = MacRUsageV2()
        if call(pid, 2, ctypes.byref(usage)):
            raise OSError(ctypes.get_errno(), "cannot measure child resources")
        return max(usage.resident_size, usage.phys_footprint), usage.diskio_bytesread
    if sys.platform.startswith("linux"):
        def numbers(path, wanted):
            output = {}
            for line in path.read_text().splitlines():
                key, value = line.split(":", 1)
                if key in wanted:
                    output[key] = int(value.split()[0])
            return output
        status = numbers(Path(f"/proc/{pid}/status"), {"VmRSS", "VmSwap"})
        disk = numbers(Path(f"/proc/{pid}/io"), {"read_bytes"})
        return (status.get("VmRSS", 0) + status.get("VmSwap", 0)) * 1024, disk["read_bytes"]
    raise RuntimeError("resource supervision requires macOS or Linux")


def swap_bytes():
    if sys.platform == "darwin":
        result = subprocess.run(
            ["/usr/sbin/sysctl", "-n", "vm.swapusage"], capture_output=True,
            text=True, timeout=2, check=True)
        found = re.search(r"used\s*=\s*([0-9.]+)([KMGTP])", result.stdout)
        if not found:
            raise RuntimeError("cannot measure system swap")
        return int(float(found[1]) * 1024 ** ("KMGTP".index(found[2]) + 1))
    if sys.platform.startswith("linux"):
        values = {key: int(value.split()[0]) for key, value in
                  (line.split(":", 1) for line in Path("/proc/meminfo").read_text().splitlines())}
        return (values["SwapTotal"] - values["SwapFree"]) * 1024
    raise RuntimeError("resource supervision requires macOS or Linux")


class Sampler:
    def __init__(self, storage_paths):
        self.volumes = [(path, path.stat().st_dev) for path in storage_paths]
        self.last_disk_sample = float("-inf")
        self.free = self.swap = None

    def system_usage(self, now):
        if now - self.last_disk_sample >= 5:
            free = []
            for path, device in self.volumes:
                if path.stat().st_dev != device:
                    raise RuntimeError("a storage volume changed or was unmounted")
                space = os.statvfs(path)
                free.append(space.f_bavail * space.f_frsize)
            self.free = min(free)
            self.swap = swap_bytes()
            self.last_disk_sample = now
        return self.swap, self.free

    def sample(self, pid, now):
        memory, reads = process_usage(pid)
        swap, free = self.system_usage(now)
        return Observation(memory, swap, free, reads)


def stop_child(child, grace_seconds=10):
    """Only call for a child started with start_new_session=True."""
    if child.poll() is not None:
        return
    try:
        os.killpg(child.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        child.wait(timeout=grace_seconds)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait()


def write_stop_receipt(path, reason, pid, observation):
    receipt = {"reason": reason, "pid": pid, "time": time.time(),
               "observation": asdict(observation) if observation is not None else None}
    fd, temporary = tempfile.mkstemp(prefix=path.name + ".", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as output:
            json.dump(receipt, output, indent=2)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def supervise(command, limits, storage_paths, stop_file, interval=0.5):
    if stop_file.exists():
        print("Atlas resource guard: an earlier stop requires operator review.", file=sys.stderr)
        return 0
    sampler = Sampler(storage_paths)
    initial_swap, free = sampler.system_usage(time.monotonic())
    if free < limits.minimum_free_bytes:
        write_stop_receipt(stop_file, "storage", None, Observation(0, initial_swap, free, 0))
        print("Atlas resource guard: insufficient free storage to launch.", file=sys.stderr)
        return 0

    stop = threading.Event()
    previous_signals = {}
    for signum in (signal.SIGTERM, signal.SIGINT):
        previous_signals[signum] = signal.signal(signum, lambda *_: stop.set())
    child = None
    reason = None
    observation = None
    try:
        child = subprocess.Popen(command, start_new_session=True)
        monitor = Monitor(limits, initial_swap)
        while child.poll() is None and not stop.is_set():
            now = time.monotonic()
            try:
                observation = sampler.sample(child.pid, now)
                reason = monitor.check(observation, now)
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
                if child.poll() is not None:
                    break
                reason = "measurement_failed"
            if reason:
                break
            stop.wait(interval)
        if child.poll() is not None and child.returncode:
            reason = reason or "child_failed"
    finally:
        if child is not None:
            stop_child(child)
        for signum, previous in previous_signals.items():
            signal.signal(signum, previous)
    if reason:
        write_stop_receipt(stop_file, reason, child.pid, observation)
        print(f"Atlas resource guard: stopped child after {reason}; operator review required.", file=sys.stderr)
    # A successful supervisor exit prevents launchd KeepAlive.SuccessfulExit=false
    # from immediately restarting a process that just crossed its resource budget.
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--max-memory-mib", type=int, default=6144)
    parser.add_argument("--max-swap-growth-mib", type=int, default=512)
    parser.add_argument("--min-free-gib", type=int, default=128)
    parser.add_argument("--max-read-mib-s", type=int, default=256)
    parser.add_argument("--read-window-seconds", type=float, default=30)
    parser.add_argument("--storage-path", action="append", type=Path, required=True)
    parser.add_argument("--stop-file", type=Path, required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("a child command is required after --")
    try:
        limits = Limits(args.max_memory_mib * MIB, args.max_swap_growth_mib * MIB,
                        args.min_free_gib * GIB, args.max_read_mib_s * MIB,
                        args.read_window_seconds)
        paths = [path.resolve(strict=True) for path in args.storage_path]
        stop_file = args.stop_file.absolute()
        if not stop_file.parent.is_dir():
            raise ValueError("stop-file parent directory must exist")
        return supervise(command, limits, paths, stop_file)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"Atlas resource guard could not run ({type(error).__name__}).", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
