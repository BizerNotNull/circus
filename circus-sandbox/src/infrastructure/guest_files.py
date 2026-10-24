"""Private Linux/Python 3 file protocol. Exit statuses are consumed by engine.rs."""
import errno
import os
import stat
import sys

UNSAFE, LIMIT, INVALID, PROTOCOL, IO_ERROR, NOT_FOUND, PERMISSION_DENIED = 81, 82, 83, 84, 85, 86, 87


def fail(code):
    raise SystemExit(code)


def components(path, absolute=False):
    if "\\" in path or "\x00" in path or (absolute != path.startswith("/")):
        fail(INVALID)
    parts = path[1:].split("/") if absolute else path.split("/")
    if any(p == ".." or (len(p) >= 2 and p[0] in
           "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ" and p[1] == ":")
           for p in parts):
        fail(INVALID)
    return [p for p in parts if p not in ("", ".")]


def open_directory(parent, name, create=False):
    if create:
        try:
            os.mkdir(name, 0o755, dir_fd=parent)
        except FileExistsError:
            pass
    return os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                   dir_fd=parent)


def walk(fd, parts, create=False):
    try:
        for part in parts:
            child = open_directory(fd, part, create)
            os.close(fd)
            fd = child
        return fd
    except BaseException:
        os.close(fd)
        raise


def main():
    mode, workspace, relative, bound_text = sys.argv[1:]
    bound = int(bound_text)
    if bound < 0:
        fail(INVALID)
    root_parts = components(workspace, True)
    parts = components(relative)
    root = walk(os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC), root_parts)
    if not parts:
        os.close(root)
        if mode == "mkdir":
            return
        fail(UNSAFE)
    parent = walk(root, parts[:-1], mode == "mkdir")
    try:
        name = parts[-1]
        if mode == "mkdir":
            os.close(open_directory(parent, name, True))
        elif mode == "unlink":
            # unlinkat never follows the final entry, even during concurrent replacement.
            os.unlink(name, dir_fd=parent)
        elif mode in ("read", "write"):
            flags = os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
            flags |= os.O_RDONLY if mode == "read" else os.O_WRONLY | os.O_CREAT
            fd = os.open(name, flags, 0o644, dir_fd=parent)
            try:
                if not stat.S_ISREG(os.fstat(fd).st_mode):
                    fail(UNSAFE)
                if mode == "read":
                    remaining = bound + 1
                    while remaining:
                        chunk = os.read(fd, min(65536, remaining))
                        if not chunk:
                            break
                        sys.stdout.buffer.write(chunk)
                        remaining -= len(chunk)
                    sys.stdout.buffer.flush()
                else:
                    # No truncation until the opened inode has been verified regular.
                    os.ftruncate(fd, 0)
                    remaining = bound
                    while remaining:
                        chunk = sys.stdin.buffer.read(min(65536, remaining))
                        if not chunk:
                            fail(PROTOCOL)
                        view = memoryview(chunk)
                        while view:
                            written = os.write(fd, view)
                            view = view[written:]
                        remaining -= len(chunk)
                    # The host frames stdin with this exact validated byte count.
                    # Do not require EOF: Windows named pipes cannot half-close.
            finally:
                os.close(fd)
        else:
            fail(PROTOCOL)
    finally:
        os.close(parent)


try:
    main()
except OSError as error:
    if error.errno in (errno.ELOOP, errno.ENOTDIR, errno.EISDIR, errno.ENXIO):
        fail(UNSAFE)
    if error.errno == errno.ENOENT:
        fail(NOT_FOUND)
    if error.errno in (errno.EACCES, errno.EPERM):
        fail(PERMISSION_DENIED)
    fail(IO_ERROR)
except (ValueError, IndexError):
    fail(PROTOCOL)
