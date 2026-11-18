#!/usr/bin/env python3
"""Starts the soak's sampler on macOS so that it outlives the application it is started from
(docs/how-to/run-the-soak.md): launch.py --context <kube-context> --out <out>.

macOS decides the access to the local network by the application responsible for a process: the
terminal or the editor the process was started from. A sampler started there and left in the
background reads the cluster while that application is open. When the application quits, the
sampler keeps running and every call to a cluster on the local network fails at once with
"no route to host", for good (docs/journal/2026-11-18-the-soak-sampler-on-macos.md).

This script starts tests/soak/sample.py as a process responsible for itself
(responsibility_spawnattrs_setdisclaim of libSystem, which the SDK's headers do not declare), in a
session of its own, with standard input from /dev/null, standard output and error appended to
<out>/sampler.log, and only HOME, USER, LOGNAME, LANG, PATH and KUBECONFIG of its own environment:
nothing else the shell holds reaches the sampler. It then starts caffeinate -i for as long as the
sampler runs, so that idle sleep does not pause it, and after two seconds says whether the sampler
still runs. --days and --interval go to sample.py as they are.

The sampler runs with the Python that runs this script, and macOS then decides the access for that
Python itself: the first time it may ask to allow it, and until it is allowed (System Settings >
Privacy & Security > Local Network) the reads fail with "no route to host". <out>/sampler.log
says why a read fails, and may name a node or the server: like the samples, it stays out of the
repository.

This script calls no cluster: sample.py checks the context. macOS only. Needs python3 (stdlib
only); run it with python3 -I.
"""

import argparse
import ctypes
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
KEPT = ("HOME", "USER", "LOGNAME", "LANG", "PATH", "KUBECONFIG")
POSIX_SPAWN_SETSID = 0x0400
CAFFEINATE = "/usr/bin/caffeinate"
SETTLE_SECONDS = 2


def spawn(libc, command, env, log, disclaim):
    """The PID of COMMAND, started in a session of its own with the environment ENV, standard
    input from /dev/null and standard output and error appended to LOG; responsible for itself
    when DISCLAIM."""
    attr, actions = ctypes.c_void_p(), ctypes.c_void_p()
    libc.posix_spawnattr_init(ctypes.byref(attr))
    libc.posix_spawn_file_actions_init(ctypes.byref(actions))
    try:
        if libc.posix_spawnattr_setflags(ctypes.byref(attr), POSIX_SPAWN_SETSID) != 0:
            raise OSError("posix_spawnattr_setflags failed")
        if disclaim and libc.responsibility_spawnattrs_setdisclaim(ctypes.byref(attr), 1) != 0:
            raise OSError("responsibility_spawnattrs_setdisclaim failed")
        append = os.O_WRONLY | os.O_CREAT | os.O_APPEND
        target = os.fsencode(log)
        libc.posix_spawn_file_actions_addopen(
            ctypes.byref(actions), 0, b"/dev/null", os.O_RDONLY, 0
        )
        libc.posix_spawn_file_actions_addopen(ctypes.byref(actions), 1, target, append, 0o600)
        libc.posix_spawn_file_actions_addopen(ctypes.byref(actions), 2, target, append, 0o600)
        argv = (ctypes.c_char_p * (len(command) + 1))(*[os.fsencode(c) for c in command], None)
        pairs = [os.fsencode("%s=%s" % item) for item in env.items()]
        envp = (ctypes.c_char_p * (len(pairs) + 1))(*pairs, None)
        pid = ctypes.c_int()
        code = libc.posix_spawn(
            ctypes.byref(pid),
            os.fsencode(command[0]),
            ctypes.byref(actions),
            ctypes.byref(attr),
            argv,
            envp,
        )
        if code != 0:
            raise OSError(code, os.strerror(code), command[0])
        return pid.value
    finally:
        libc.posix_spawn_file_actions_destroy(ctypes.byref(actions))
        libc.posix_spawnattr_destroy(ctypes.byref(attr))


def main():
    parser = argparse.ArgumentParser(
        prog="tests/soak/launch.py",
        description="Start the soak's sampler on macOS (docs/how-to/run-the-soak.md).",
    )
    parser.add_argument("--context", required=True, help="the kube context of the soak's cluster")
    parser.add_argument("--out", required=True, help="the output directory, outside the repository")
    parser.add_argument("--days", help="days to sample, for tests/soak/sample.py")
    parser.add_argument("--interval", help="seconds between samples, for tests/soak/sample.py")
    args = parser.parse_args()
    if not args.context or args.context.startswith("-"):
        parser.error("--context must name a context of the kubeconfig")
    if not args.out:
        parser.error("--out must name a directory")
    if sys.platform != "darwin":
        print(
            "launch.py: macOS only: on another system, run tests/soak/sample.py itself",
            file=sys.stderr,
        )
        return 1
    try:
        libc = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
        libc.posix_spawn_file_actions_addopen.argtypes = [
            ctypes.c_void_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_uint16,
        ]
        libc.posix_spawnattr_setflags.argtypes = [ctypes.c_void_p, ctypes.c_short]
        libc.responsibility_spawnattrs_setdisclaim.argtypes = [ctypes.c_void_p, ctypes.c_int]
    except (OSError, AttributeError) as error:
        print("launch.py: this macOS lacks a call the launcher needs: %s" % error, file=sys.stderr)
        return 1
    out = os.path.abspath(args.out)
    os.makedirs(out, exist_ok=True)
    log = os.path.join(out, "sampler.log")
    command = [sys.executable, "-I", os.path.join(HERE, "sample.py")]
    command += ["--context", args.context, "--out", out]
    for flag, value in (("--days", args.days), ("--interval", args.interval)):
        if value is not None:
            command += [flag, value]
    env = {name: os.environ[name] for name in KEPT if name in os.environ}
    try:
        sampler = spawn(libc, command, env, log, True)
    except OSError as error:
        print("launch.py: could not start the sampler: %s" % error, file=sys.stderr)
        return 1
    try:
        spawn(libc, [CAFFEINATE, "-i", "-w", str(sampler)], env, "/dev/null", False)
    except OSError as error:
        print("launch.py: no caffeinate, idle sleep will pause the sampler: %s" % error)
    time.sleep(SETTLE_SECONDS)
    ended, status = os.waitpid(sampler, os.WNOHANG)
    if ended:
        print(
            "launch.py: the sampler ended at once, with %d: see %s"
            % (os.waitstatus_to_exitcode(status), log),
            file=sys.stderr,
        )
        return 1
    print(
        "launch.py: the sampler runs, PID %d; its samples and sampler.log are in %s"
        % (sampler, out)
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
