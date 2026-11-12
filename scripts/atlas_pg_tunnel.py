#!/usr/bin/env python3
"""Supervise one private PostgreSQL SSH Unix forward under atlas_resource_guard.

Deploy beside atlas_resource_guard.py. The outer guard owns this process group;
this helper measures its direct, nonforking SSH child and never makes a new group.
"""
import argparse
import collections
import importlib.util
import os
from pathlib import Path
import re
import signal
import socket
import stat
import subprocess
import sys
import threading
import time

_spec = importlib.util.spec_from_file_location(
    "_atlas_tunnel_resource_guard", Path(__file__).with_name("atlas_resource_guard.py"))
guard = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = guard
_spec.loader.exec_module(guard)

MAX_MEMORY = 128 * (1 << 20)
SAMPLE_SECONDS = 0.5
TAIL_BYTES = 16384


class Refused(RuntimeError):
    pass


def inode(value):
    return value.st_dev, value.st_ino, value.st_uid, stat.S_IFMT(value.st_mode)


class SocketLease:
    def __init__(self, path):
        provided = Path(path).absolute()
        self.parent = provided.parent.resolve(strict=True)
        self.path = self.parent / provided.name
        parent = self.parent.lstat()
        if not stat.S_ISDIR(parent.st_mode) or parent.st_uid != os.getuid() or parent.st_mode & 0o077:
            raise Refused("forward parent must be an owned private directory")
        if ":" in str(self.path) or len(os.fsencode(self.path)) > 100:
            raise Refused("unsupported forward path")
        self.directory = inode(parent)
        self.owned = None
        if self.current() is not None:
            raise Refused("initial forward path already exists")

    def current(self):
        if inode(self.parent.lstat()) != self.directory:
            raise Refused("forward directory changed")
        try:
            return self.path.lstat()
        except FileNotFoundError:
            return None

    def capture(self):
        current = self.current()
        if current is None:
            if self.owned is not None:
                raise Refused("live forward socket disappeared")
            return
        if not stat.S_ISSOCK(current.st_mode) or current.st_uid != os.getuid():
            raise Refused("unknown forward content")
        if self.owned is not None and inode(current) != self.owned:
            raise Refused("forward socket changed")
        self.owned = inode(current)

    def cleanup(self):
        # Only after the exact child has been reaped. Never use SSH's blind
        # StreamLocalBindUnlink, or remove an initial/changed/live socket.
        current = self.current()
        if current is None:
            self.owned = None
            return
        if self.owned is None or inode(current) != self.owned:
            raise Refused("unowned forward path remains")
        with socket.socket(socket.AF_UNIX) as probe:
            probe.settimeout(0.2)
            try:
                probe.connect(str(self.path))
            except ConnectionRefusedError:
                pass
            except FileNotFoundError:
                return
            except OSError as error:
                raise Refused("cannot verify dead forward listener") from error
            else:
                raise Refused("a live forward listener remains")
        current = self.current()
        if current is None:
            self.owned = None
            return
        if inode(current) != self.owned:
            raise Refused("forward socket changed during cleanup")
        self.path.unlink()
        self.owned = None


def fatal_diagnostic(message):
    message = message.lower()
    fatal = ("permission denied", "host key verification failed", "identification has changed",
             "bad permissions", "not accessible", "invalid format", "bad configuration",
             "bad configuration option", "unknown option", "address already in use",
             "cannot listen", "cannot bind", "bind:", "authentication failed",
             "too many authentication failures", "no supported authentication methods",
             "load key", "bad owner or permissions", "request local forwarding")
    return any(value in message for value in fatal)


class Diagnostics:
    def __init__(self):
        self.tail = collections.deque(maxlen=TAIL_BYTES)
        self.fatal = False

    def add(self, part):
        self.tail.extend(part)
        self.fatal = self.fatal or fatal_diagnostic(bytes(self).decode("utf-8", "replace"))

    def __bytes__(self):
        return bytes(self.tail)

    def __len__(self):
        return len(self.tail)


def transient_exit(code, tail):
    message = bytes(tail).decode("utf-8", "replace").lower()
    if getattr(tail, "fatal", False) or fatal_diagnostic(message):
        return False
    transient = ("connection refused", "connection timed out", "operation timed out",
                 "connection reset", "connection closed", "broken pipe", "no route to host",
                 "network is unreachable", "server not responding", "timeout, server",
                 "temporary failure in name resolution")
    return code in (0, 255) and (any(value in message for value in transient)
                                  or re.search(r"connection to [^\r\n]+ closed", message) is not None)


def stop_child(child, grace=3):
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=grace)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=2)
    else:
        child.wait()


def drain(child, tail):
    # Continuous stderr must not starve cancellation or resource sampling.
    for _ in range(16):
        try:
            part = os.read(child.stderr.fileno(), 4096)
        except BlockingIOError:
            return
        if not part:
            return
        tail.add(part)


def supervise(command, socket_path, stop_file, stop, *, initial_backoff=1, max_backoff=30,
              memory_limit=MAX_MEMORY, sample_seconds=SAMPLE_SECONDS, grace=3, observer=None):
    """Internal command injection is for owned tests; CLI only builds fixed SSH arguments."""
    stop_file = Path(stop_file).absolute()
    child = None
    lease = None
    reason = "supervising"
    fenced = False
    paused = False
    backoff = initial_backoff
    try:
        if stop_file.exists():
            raise Refused("an earlier tunnel stop requires operator review")
        lease = SocketLease(socket_path)
        parent = stop_file.parent.stat()
        if not stat.S_ISDIR(parent.st_mode) or parent.st_uid != os.getuid() or parent.st_mode & 0o077:
            raise Refused("stop-file parent must be an owned private directory")
        guard.write_stop_receipt(stop_file, reason, None, None, exclusive=True)
        fenced = True
        while not stop.is_set():
            if lease.current() is not None:
                raise Refused("forward path appeared while paused")
            # Inherit the outer guarded group. -F /dev/null and fixed options
            # prohibit proxies, multiplexers, daemonization and local commands.
            child = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.PIPE, start_new_session=False, close_fds=True)
            os.set_blocking(child.stderr.fileno(), False)
            tail = Diagnostics()
            sampled = float("-inf")
            while child.poll() is None and not stop.is_set():
                drain(child, tail)
                lease.capture()
                now = time.monotonic()
                if now - sampled >= sample_seconds:
                    sampled = now
                    try:
                        memory, _ = guard.process_usage(child.pid)
                    except (OSError, ValueError, RuntimeError):
                        if child.poll() is not None:
                            break
                        reason = "measurement_failed"
                        raise Refused("cannot measure SSH child")
                    if observer is not None:
                        observer(child.pid, memory, len(tail))
                    if memory > memory_limit:
                        reason = "memory"
                        raise Refused("SSH child memory limit exceeded")
                if lease.owned is not None and paused:
                    print("Atlas PostgreSQL tunnel: forward restored.", file=sys.stderr)
                    paused = False
                    backoff = initial_backoff
                stop.wait(min(sample_seconds, 0.05))
            if child.poll() is None:
                lease.capture()
            stop_child(child, grace)
            drain(child, tail)
            code = child.returncode
            child.stderr.close()
            lease.cleanup()
            child = None
            if stop.is_set():
                break
            if not transient_exit(code, tail):
                reason = "ssh_failed"
                raise Refused("SSH exited without a recognized transient transport failure")
            if not paused:
                print("Atlas PostgreSQL tunnel: transport unavailable; waiting.", file=sys.stderr)
                paused = True
            stop.wait(backoff)
            backoff = min(backoff * 2, max_backoff)
        reason = None
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        if reason == "supervising":
            reason = "ownership_or_configuration"
        print("Atlas PostgreSQL tunnel: stopped; operator review required.", file=sys.stderr)
    finally:
        try:
            if child is not None:
                stop_child(child, grace)
                child.stderr.close()
                if lease is not None:
                    lease.cleanup()
        except (OSError, RuntimeError, subprocess.SubprocessError):
            reason = reason or "cleanup_failed"
        if fenced:
            if reason is None:
                stop_file.unlink()
            else:
                # The initial exclusive fence remains if the update fails.
                try:
                    guard.write_stop_receipt(stop_file, reason, child.pid if child else None, None)
                except OSError:
                    pass

    return int(reason is not None)


def build_command(args):
    host = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.:-]*\Z")
    user = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_.-]*\Z")
    if not host.fullmatch(args.host) or not host.fullmatch(args.remote_host) or not user.fullmatch(args.user):
        raise ValueError("invalid explicit SSH host/user")
    if not 1 <= args.port <= 65535 or not 1 <= args.remote_port <= 65535:
        raise ValueError("invalid port")
    key = args.identity_file.resolve(strict=True)
    known = args.known_hosts_file.resolve(strict=True)
    if not key.is_file() or key.stat().st_uid != os.getuid() or key.stat().st_mode & 0o077 or not known.is_file():
        raise ValueError("invalid SSH identity or known-hosts file")
    remote = "[" + args.remote_host + "]" if ":" in args.remote_host else args.remote_host
    forward = str(args.socket.absolute()) + ":" + remote + ":" + str(args.remote_port)
    command = ["/usr/bin/ssh", "-F", "/dev/null", "-N", "-T", "-n", "-l", args.user,
               "-p", str(args.port), "-i", str(key)]
    # OpenSSH emits keepalive timeouts at INFO; transient_exit needs that diagnostic.
    for option in ("BatchMode=yes", "StrictHostKeyChecking=yes", "UserKnownHostsFile=" + str(known),
                   "GlobalKnownHostsFile=/dev/null", "IdentitiesOnly=yes", "ExitOnForwardFailure=yes",
                   "ControlMaster=no", "ControlPath=none", "ControlPersist=no", "ForkAfterAuthentication=no",
                   "ProxyCommand=none", "ProxyJump=none", "PermitLocalCommand=no", "ForwardAgent=no",
                   "ForwardX11=no", "StreamLocalBindUnlink=no", "StreamLocalBindMask=0177",
                   "ConnectTimeout=5", "ConnectionAttempts=1", "ServerAliveInterval=5",
                   "ServerAliveCountMax=3", "LogLevel=INFO"):
        command.extend(["-o", option])
    return command + ["-L", forward, "--", args.host]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", required=True)
    parser.add_argument("--user", required=True)
    parser.add_argument("--port", type=int, default=22)
    parser.add_argument("--identity-file", type=Path, required=True)
    parser.add_argument("--known-hosts-file", type=Path, required=True)
    parser.add_argument("--remote-host", required=True)
    parser.add_argument("--remote-port", type=int, default=5432)
    parser.add_argument("--socket", type=Path, required=True)
    parser.add_argument("--stop-file", type=Path, required=True)
    args = parser.parse_args()
    stop = threading.Event()
    for signum in (signal.SIGTERM, signal.SIGINT):
        signal.signal(signum, lambda *_: stop.set())
    try:
        command = build_command(args)
    except (OSError, ValueError):
        print("Atlas PostgreSQL tunnel: invalid explicit SSH configuration.", file=sys.stderr)
        return 1
    return supervise(command, args.socket, args.stop_file, stop)


if __name__ == "__main__":
    raise SystemExit(main())
