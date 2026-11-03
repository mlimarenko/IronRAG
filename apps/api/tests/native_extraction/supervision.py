"""Linux invocation ownership for the real native qualification process tree."""

import ctypes
import os
from pathlib import Path
import resource
import signal
import subprocess
import tempfile
import threading
import time


class SupervisionFailure(RuntimeError):
    """The parent could not prove termination or private-scope cleanup."""


def _subreaper(value=None):
    # Linux prctl.h: GET_CHILD_SUBREAPER=37, SET_CHILD_SUBREAPER=36.
    libc = ctypes.CDLL(None, use_errno=True)
    if value is None:
        current = ctypes.c_int()
        result = libc.prctl(37, ctypes.byref(current), 0, 0, 0)
        value = current.value
    else:
        result = libc.prctl(36, int(value), 0, 0, 0)
    if result != 0:
        raise OSError(ctypes.get_errno(), "child-subreaper prctl failed")
    return value


def _owned_children():
    pid = os.getpid()
    raw = Path(f"/proc/{pid}/task/{pid}/children").read_bytes()
    if len(raw) > 8192:
        raise SupervisionFailure("excessive adopted child inventory")
    return [int(value) for value in raw.split()]


def run_owned(command, request, directory, *, timeout=300, grace=5):
    """Reap the root and adopted descendants before checked scratch disposal."""
    directory = directory.resolve()
    if _owned_children():
        raise SupervisionFailure("qualification supervisor already owns another child")
    original_subreaper = _subreaper()
    scope = tempfile.TemporaryDirectory(prefix="scratch-", dir=directory)
    temporary = Path(scope.name)
    started = time.monotonic()
    root_done = threading.Event()
    descendants_done = threading.Event()
    observed = {}
    reaped = []
    errors = []
    child = None
    waiter = None
    descendant_waiter = None
    timed_out = False

    def limits():
        resource.setrlimit(resource.RLIMIT_FSIZE, (16 * 1024**2, 16 * 1024**2))
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        resource.setrlimit(resource.RLIMIT_NOFILE, (128, 128))
        resource.setrlimit(resource.RLIMIT_AS, (8 * 1024**3, 8 * 1024**3))

    def reap_root():
        try:
            _, status, usage = os.wait4(child.pid, 0)
            observed.update(status=status, peakRssKiB=usage.ru_maxrss)
        except BaseException as error:
            errors.append(error)
        finally:
            root_done.set()

    def reap_descendants():
        try:
            # The root wait owner has finished. All remaining children of this
            # isolated supervisor are adopted descendants of this invocation.
            while True:
                for pid in _owned_children():
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                try:
                    pid, status = os.waitpid(-1, 0)
                except ChildProcessError:
                    break
                reaped.append({"pid": pid, "exit": os.waitstatus_to_exitcode(status)})
        except BaseException as error:
            errors.append(error)
        finally:
            descendants_done.set()

    def close_scope():
        try:
            scope.cleanup()
        except OSError as error:
            raise SupervisionFailure("checked private-scope cleanup failed") from error
        if temporary.exists():
            raise SupervisionFailure("private scope remains after checked cleanup")

    try:
        _subreaper(True)
        env = {
            "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "TMPDIR": str(temporary),
            "HOME": str(temporary), "XDG_CACHE_HOME": str(temporary / "cache"),
            "RAYON_NUM_THREADS": "1", "OMP_NUM_THREADS": "1", "OMP_THREAD_LIMIT": "1",
        }
        with (directory / "stdout.json").open("xb") as stdout, (directory / "stderr.out").open("xb") as stderr:
            child = subprocess.Popen(
                command, stdin=subprocess.PIPE, stdout=stdout, stderr=stderr,
                start_new_session=True, preexec_fn=limits, env=env, cwd=temporary,
            )
            waiter = threading.Thread(target=reap_root, daemon=True)
            waiter.start()
            try:
                child.stdin.write(request)
                child.stdin.close()
            except BrokenPipeError:
                child.stdin.close()
            timed_out = not root_done.wait(max(0, timeout - (time.monotonic() - started)))
            if timed_out:
                try:
                    os.killpg(child.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                if not root_done.wait(grace):
                    try:
                        os.killpg(child.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
            if not root_done.wait(grace):
                raise SupervisionFailure("native root did not reap after termination")
            waiter.join()
            if errors:
                raise SupervisionFailure("root wait failed") from errors[0]
            child.returncode = os.waitstatus_to_exitcode(observed["status"])
            descendant_waiter = threading.Thread(target=reap_descendants, daemon=True)
            descendant_waiter.start()
            if not descendants_done.wait(grace):
                raise SupervisionFailure("adopted descendants did not reap")
            descendant_waiter.join()
            if errors:
                raise SupervisionFailure("descendant wait failed") from errors[0]
            if _owned_children():
                raise SupervisionFailure("children remain after reaping")
            try:
                os.killpg(child.pid, 0)
            except ProcessLookupError:
                pass
            else:
                raise SupervisionFailure("native process group remains after reaping")
        # All process writers and retained-output handles are closed first.
        entries = [path.name for path in temporary.iterdir()]
        close_scope()
        return {
            "exit": child.returncode, "timeout": timed_out,
            "elapsedSeconds": time.monotonic() - started,
            "temporaryEntriesBeforeCleanup": entries, "temporaryEntries": [],
            "scratchPath": str(temporary), "scratchRemoved": True,
            "peakRssKiB": observed["peakRssKiB"], "addressLimitBytes": 8 * 1024**3,
            "documentDeadlineSeconds": timeout,
            "waitCompleted": True, "descendantsReaped": reaped,
            "remainingChildren": [], "processGroupGone": True,
        }
    finally:
        # Also finalize on parsing/wait errors or parent cancellation. A failed
        # termination/cleanup raises rather than stamping a release receipt.
        if child is not None:
            if waiter is None:
                waiter = threading.Thread(target=reap_root, daemon=True)
                waiter.start()
            if not root_done.is_set():
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                if not root_done.wait(grace):
                    raise SupervisionFailure("root remains during finalization")
            waiter.join()
            if "status" in observed:
                child.returncode = os.waitstatus_to_exitcode(observed["status"])
            if not child.stdin.closed:
                try:
                    child.stdin.close()
                except BrokenPipeError:
                    pass
            if descendant_waiter is None:
                descendant_waiter = threading.Thread(target=reap_descendants, daemon=True)
                descendant_waiter.start()
            if not descendants_done.wait(grace):
                raise SupervisionFailure("descendants remain during finalization")
            descendant_waiter.join()
            if errors or _owned_children():
                raise SupervisionFailure("process-tree finalization failed")
        try:
            if temporary.exists():
                close_scope()
        finally:
            _subreaper(original_subreaper)
