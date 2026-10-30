"""Tests of install.sh (story "One-line install on macOS and Linux: ddc.sh/install.sh
and cargo", DDC-007): python3 -I -m unittest discover -s packaging/install -p 'test_*.py'.

Every case runs the script against a fake release served on 127.0.0.1, with a
temporary HOME, --prefix and TMPDIR, PATH=<fake bin>:/usr/bin:/bin and
DDC_INSTALL_APP_PATH, so the machine's own installs are never seen or touched. The
fake bin holds uname, sw_vers and sysctl, so one machine plays every platform. The
script runs under dash where there is one, and once through `cat install.sh | sh`.

The macOS cases need codesign: their stub is stub.c, signed ad hoc. The Linux cases run
everywhere with a shell-script stub. The one case that creates a real LaunchAgent runs
only on a GitHub macOS runner that asks for it (GITHUB_ACTIONS=true and
DDC_INSTALL_REAL_AGENT=1, the Install workflow): its label is the one every
`ddc schedule install` uses, so on a Mac it would replace the owner's agent.
"""
import atexit
import concurrent.futures
import functools
import hashlib
import http.server
import io
import os
import plistlib
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import unittest
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
SCRIPT = os.path.join(HERE, "install.sh")
MAC = sys.platform == "darwin"
LINUX = sys.platform.startswith("linux")
REAL_AGENT = MAC and os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("DDC_INSTALL_REAL_AGENT") == "1"
DASH = next((p for p in ("/bin/dash", "/usr/bin/dash") if os.access(p, os.X_OK)), None)
SHELL = DASH or "/bin/sh"
DOCS = ("LICENSE.md", "DISCLAIMER.md", "README.md")
DARWIN = "aarch64-apple-darwin"
MUSL = "x86_64-unknown-linux-musl"
BUILD = tempfile.mkdtemp(prefix="ddc-install-stubs.")
atexit.register(shutil.rmtree, BUILD, True)
STUB_LOCK = threading.Lock()
# Seconds for one run. macOS checks every freshly written executable on its first run,
# one at a time: under a busy machine (a build beside the gate) that can take minutes.
TIMEOUT = 300


def read(*path):
    with open(os.path.join(ROOT, *path), encoding="utf-8") as f:
        return f.read()


def constant(text, name):
    """The value of a top-level NAME=value line."""
    found = re.findall(rf"^{name}=(\S+)", text, re.M)
    assert len(found) == 1, f"{name}: {found}"
    return found[0]


CLI_ID = constant(read("packaging/install/install.sh"), "CLI_ID")
LABEL = constant(read("packaging/install/install.sh"), "LABEL")


def mac_stub(version, identifier=CLI_ID):
    with STUB_LOCK:
        return _mac_stub(version, identifier)


@functools.lru_cache(maxsize=None)
def _mac_stub(version, identifier):
    """stub.c built for `version` and signed ad hoc as `identifier`."""
    out = os.path.join(BUILD, f"stub-{version}-{identifier}")
    subprocess.run(["cc", f'-DVERSION="{version}"', "-o", out, os.path.join(HERE, "stub.c")], check=True)
    subprocess.run(["/usr/bin/codesign", "-f", "-s", "-", "-i", identifier, out], check=True, capture_output=True)
    with open(out, "rb") as f:
        return f.read()


def sh_stub(version):
    return (
        "#!/bin/sh\n"
        f'case "$1" in --version) echo "deepdrivecleaner {version}" ;;\n'
        f'  wait) read -r _; echo "deepdrivecleaner {version} waited" ;;\n'
        f'  *) echo "deepdrivecleaner {version} ran" ;; esac\n'
    ).encode()


def archive(binary, extra=()):
    """A release archive: the binary and the three documents, flat, as build-dist.sh packs it."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:

        def add(name, data, mode):
            info = tarfile.TarInfo(name)
            info.size, info.mode, info.mtime = len(data), mode, 0
            tar.addfile(info, io.BytesIO(data))

        add("deepdrivecleaner", binary, 0o755)
        for name in DOCS + tuple(extra):
            add(name, b"text\n", 0o644)
    return buf.getvalue()


def sha256(data):
    return hashlib.sha256(data).hexdigest()


class FakeRelease:
    """GitHub's release layout on 127.0.0.1: <url>/download/v<v>/<asset>, and
    <url>/latest redirecting to <url>/tag/v<latest> when `latest` is set."""

    def __init__(self):
        self.root = tempfile.mkdtemp(prefix="ddc-install-release.")
        self.requests = []
        self.latest = None
        release = self

        class Handler(http.server.SimpleHTTPRequestHandler):
            def __init__(self, *a, **k):
                super().__init__(*a, directory=release.root, **k)

            def log_message(self, *a):
                pass

            def do_GET(self):
                release.requests.append(self.path)
                if self.path == "/latest" and release.latest:
                    self.send_response(302)
                    self.send_header("Location", f"{release.url}/tag/v{release.latest}")
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                elif self.path.startswith("/tag/"):
                    self.send_response(200)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                else:
                    super().do_GET()

        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.httpd.server_address[1]}"
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()

    def close(self):
        self.httpd.shutdown()
        self.httpd.server_close()
        shutil.rmtree(self.root, ignore_errors=True)

    def publish(self, version, archives, sums=True, served=None):
        """`archives` {target: bytes}; SHA256SUMS over them (True), none (False) or the
        given text; `served` {target: bytes} replaces what is served after summing."""
        folder = os.path.join(self.root, "download", f"v{version}")
        os.makedirs(folder, exist_ok=True)
        names = {}
        for target, data in archives.items():
            names[target] = f"deepdrivecleaner-{version}-{target}.tar.gz"
            with open(os.path.join(folder, names[target]), "wb") as f:
                f.write((served or {}).get(target, data))
        if sums is True:
            sums = "".join(f"{sha256(archives[t])}  {names[t]}\n" for t in sorted(archives, key=lambda t: names[t]))
        if sums:
            with open(os.path.join(folder, "SHA256SUMS"), "w") as f:
                f.write(sums)

    def archive_requests(self):
        return [r for r in self.requests if r.endswith(".tar.gz")]


def chmod_tree_writable(path):
    for dirpath, dirs, _ in os.walk(path):
        for d in dirs:
            os.chmod(os.path.join(dirpath, d), 0o755)
    os.chmod(path, 0o755)


class Case(unittest.TestCase):
    def setUp(self):
        self.tmp = os.path.realpath(tempfile.mkdtemp(prefix="ddc-install-test."))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.addCleanup(chmod_tree_writable, self.tmp)
        self.home = os.path.join(self.tmp, "home")
        self.prefix = os.path.join(self.home, ".local", "bin")
        self.tmpdir = os.path.join(self.tmp, "t")
        self.fakebin = os.path.join(self.tmp, "fakebin")
        self.app = os.path.join(self.tmp, "Applications", "DeepDriveCleaner.app")
        for d in (self.home, self.tmpdir, self.fakebin):
            os.makedirs(d)
        self.release = FakeRelease()
        self.addCleanup(self.release.close)
        self.path = f"{self.fakebin}:/usr/bin:/bin"
        self.platform("Darwin", "arm64")

    def fake(self, name, body):
        path = os.path.join(self.fakebin, name)
        with open(path, "w") as f:
            f.write("#!/bin/sh\n" + body)
        os.chmod(path, 0o755)

    def platform(self, system, arch, macos="26.1", translated=None):
        self.fake("uname", f'case "$1" in -s) echo {system} ;; -m) echo {arch} ;; *) echo {system} ;; esac\n')
        self.fake("sw_vers", f"echo {macos}\n")
        if translated is None:
            self.fake("sysctl", "echo 'sysctl: unknown oid' >&2; exit 1\n")
        else:
            self.fake("sysctl", f"echo {translated}\n")

    def env(self, **over):
        env = {
            "PATH": self.path,
            "HOME": self.home,
            "TMPDIR": self.tmpdir,
            "SHELL": "/bin/zsh",
            "DDC_INSTALL_BASE_URL": self.release.url,
            "DDC_INSTALL_ALLOW_ADHOC": "1",
            "DDC_INSTALL_APP_PATH": self.app,
        }
        env.update(over)
        return {k: v for k, v in env.items() if v is not None}

    def install(self, *args, shell=SHELL, piped=False, cwd=None, **env):
        if piped:
            cmd = ["/bin/sh", "-c", f'cat "{SCRIPT}" | /bin/sh -s -- "$@"', "sh", *args]
        else:
            cmd = [shell, SCRIPT, *args]
        return subprocess.run(cmd, env=self.env(**env), cwd=cwd, capture_output=True, text=True, timeout=TIMEOUT)

    def ok(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def fails(self, result, code, *needles):
        self.assertEqual(result.returncode, code, result.stdout + result.stderr)
        for needle in needles:
            self.assertIn(needle, result.stderr)
        return result

    def run_bin(self, *args):
        return subprocess.run(list(args), capture_output=True, text=True, timeout=TIMEOUT, env={"PATH": "/usr/bin:/bin"})

    def assertNothingLeft(self):
        self.assertEqual(os.listdir(self.tmpdir), [], "the temporary folder is removed")
        if os.path.isdir(self.prefix):
            staged = [n for n in os.listdir(self.prefix) if ".new." in n]
            self.assertEqual(staged, [], "no staged file is left in --prefix")

    def assertInstalled(self, version):
        dest = os.path.join(self.prefix, "deepdrivecleaner")
        link = os.path.join(self.prefix, "ddc")
        self.assertEqual(sorted(os.listdir(self.prefix)), ["ddc", "deepdrivecleaner"])
        self.assertTrue(os.path.islink(link))
        self.assertEqual(os.readlink(link), dest)
        self.assertEqual(stat.S_IMODE(os.stat(dest).st_mode), 0o755)
        for exe in (dest, link):
            self.assertEqual(self.run_bin(exe, "--version").stdout, f"deepdrivecleaner {version}\n")
        self.assertNothingLeft()


@unittest.skipUnless(MAC, "the macOS cases sign their stub with codesign")
class MacInstall(Case):
    def publish(self, version, binary=None, **kw):
        self.release.publish(version, {DARWIN: archive(binary or mac_stub(version))}, **kw)

    def test_a_fresh_install_prints_the_path_line(self):
        self.publish("0.8.0")
        r = self.ok(self.install("--version", "0.8.0"))
        self.assertInstalled("0.8.0")
        self.assertIn(f'  export PATH="{self.prefix}:$PATH"\n', r.stdout)
        self.assertIn("Full Disk Access", r.stdout)
        self.assertIn("WARNING: test mode", r.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.home, ".zshrc")), "no profile is edited")

    def test_piped_into_sh(self):
        self.publish("0.8.0")
        self.ok(self.install("--version", "0.8.0", piped=True))
        self.assertInstalled("0.8.0")

    def test_under_the_system_sh(self):
        self.publish("0.8.0")
        self.ok(self.install("--version", "v0.8.0", shell="/bin/sh"))
        self.assertInstalled("0.8.0")

    def test_the_latest_release_by_its_redirect(self):
        self.publish("0.8.0")
        self.publish("0.8.1")
        self.release.latest = "0.8.1"
        self.ok(self.install())
        self.assertInstalled("0.8.1")

    def test_no_latest_redirect_is_a_download_failure(self):
        self.fails(self.install(), 5, "cannot find the latest release")
        self.assertFalse(os.path.exists(self.prefix))

    def test_an_upgrade_keeps_the_path_a_running_copy_and_the_agent(self):
        self.publish("0.8.0")
        self.publish("0.8.1")
        self.ok(self.install("--version", "0.8.0"))
        dest = os.path.join(self.prefix, "deepdrivecleaner")
        # The agent `ddc schedule install` writes (crates/ddc-cli/src/launchd.rs).
        agents = os.path.join(self.home, "Library", "LaunchAgents")
        os.makedirs(agents)
        with open(os.path.join(agents, f"{LABEL}.plist"), "wb") as f:
            plistlib.dump({"Label": LABEL, "ProgramArguments": [dest, "scan", "--background"]}, f)
        running = subprocess.Popen([dest, "wait"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        self.addCleanup(running.kill)

        r = self.ok(self.install("--version", "0.8.1"))

        self.assertIn("deepdrivecleaner 0.8.0 -> 0.8.1", r.stdout)
        self.assertIsNone(running.poll(), "the old copy still runs")
        self.assertEqual(running.communicate("go\n", timeout=TIMEOUT)[0], "deepdrivecleaner 0.8.0 waited\n")
        self.assertEqual(running.returncode, 0)
        self.assertInstalled("0.8.1")
        with open(os.path.join(agents, f"{LABEL}.plist"), "rb") as f:
            program = plistlib.load(f)["ProgramArguments"]
        self.assertEqual(self.run_bin(*program).stdout, "deepdrivecleaner 0.8.1 ran\n")

    def test_an_install_on_path_upgrades_without_a_path_line(self):
        self.publish("0.8.0")
        self.publish("0.8.1")
        self.path = f"{self.fakebin}:{self.prefix}:/usr/bin:/bin"
        self.ok(self.install("--version", "0.8.0"))
        r = self.ok(self.install("--version", "0.8.1"))
        self.assertInstalled("0.8.1")
        self.assertNotIn("export PATH", r.stdout)

    def test_relative_path_entries_are_skipped(self):
        # They once sent the folder walk of canon_path into an endless loop.
        self.publish("0.8.0")
        os.makedirs(os.path.join(self.home, "bin"))
        self.path = f"nope/bin:.git/safe/../../bin:bin:{self.path}"
        r = self.ok(self.install("--version", "0.8.0", cwd=self.home))
        self.assertInstalled("0.8.0")
        self.assertIn(f'  export PATH="{self.prefix}:$PATH"\n', r.stdout)
        r = self.ok(self.install("--version", "0.8.0", cwd=self.home))
        self.assertIn("already current", r.stdout)

    def test_already_current_downloads_nothing(self):
        self.publish("0.8.0")
        self.ok(self.install("--version", "0.8.0"))
        before = len(self.release.archive_requests())
        r = self.ok(self.install("--version", "0.8.0"))
        self.assertIn("already current", r.stdout)
        self.assertEqual(len(self.release.archive_requests()), before)
        self.ok(self.install("--version", "0.8.0", "--force"))
        self.assertEqual(len(self.release.archive_requests()), before + 1, "--force installs again")
        self.assertInstalled("0.8.0")

    def test_a_tampered_archive_installs_nothing(self):
        good = archive(mac_stub("0.8.0"))
        bad = archive(mac_stub("0.8.0"), extra=())[:-1] + b"\x01"
        self.release.publish("0.8.0", {DARWIN: good}, served={DARWIN: bad})
        self.fails(self.install("--version", "0.8.0"), 3, "does not match SHA256SUMS")
        self.assertFalse(os.path.exists(self.prefix), "--prefix is never created")
        self.assertNothingLeft()

    def test_a_tampered_sums_file_installs_nothing(self):
        name = f"deepdrivecleaner-0.8.0-{DARWIN}.tar.gz"
        self.publish("0.8.0", sums=f"{'0' * 64}  {name}\n")
        self.fails(self.install("--version", "0.8.0"), 3, "does not match SHA256SUMS")
        self.assertFalse(os.path.exists(self.prefix))
        self.assertNothingLeft()

    def test_an_archive_with_more_files_is_refused(self):
        self.release.publish("0.8.0", {DARWIN: archive(mac_stub("0.8.0"), extra=["../evil"])})
        self.fails(self.install("--version", "0.8.0"), 3, "not the four expected files")
        self.assertFalse(os.path.exists(self.prefix))
        self.assertNothingLeft()

    def test_another_identifier_is_refused(self):
        self.publish("0.8.0", binary=mac_stub("0.8.0", "com.example.other"))
        self.fails(self.install("--version", "0.8.0"), 3, "not signed as")
        self.assertFalse(os.path.exists(self.prefix))

    def test_an_adhoc_signature_fails_outside_test_mode(self):
        # The loopback host alone is no test mode: the Developer ID team is required.
        self.publish("0.8.0")
        r = self.install("--version", "0.8.0", "--force", DDC_INSTALL_ALLOW_ADHOC=None, DDC_INSTALL_APP_PATH=None)
        self.fails(r, 3, "by the SmartAndPoint Developer ID")
        self.assertNotIn("test mode", r.stderr)
        self.assertFalse(os.path.exists(self.prefix))
        self.assertNothingLeft()

    def test_a_binary_of_another_version_is_refused(self):
        self.publish("0.8.0", binary=mac_stub("0.7.9"))
        self.fails(self.install("--version", "0.8.0"), 3, "reports 'deepdrivecleaner 0.7.9'")
        self.assertFalse(os.path.exists(self.prefix))

    def test_a_release_without_sums_names_the_first_verifiable_version(self):
        self.publish("0.3.0", sums=False)
        first = constant(read("packaging/install/install.sh"), "FIRST_VERIFIABLE")
        self.fails(self.install("--version", "0.3.0"), 3, f"releases from {first} on carry one")
        self.assertFalse(os.path.exists(self.prefix))

    def test_a_missing_release_is_a_download_failure(self):
        self.fails(self.install("--version", "9.9.9"), 5, "no release v9.9.9")
        self.assertNothingLeft()

    def existing(self, folder, name="deepdrivecleaner", target=None):
        os.makedirs(folder, exist_ok=True)
        path = os.path.join(folder, name)
        if target:
            os.makedirs(os.path.dirname(target), exist_ok=True)
            with open(target, "w") as f:
                f.write("#!/bin/sh\n")
            os.symlink(target, path)
        else:
            with open(path, "w") as f:
                f.write("#!/bin/sh\n")
        os.chmod(path, 0o755)
        self.path = f"{self.fakebin}:{folder}:/usr/bin:/bin"
        return path

    def test_a_homebrew_cask_install_is_named(self):
        self.publish("0.8.0")
        cask = os.path.join(self.tmp, "opt/homebrew/Caskroom/deepdrivecleaner/0.7.0/DeepDriveCleaner.app")
        path = self.existing(
            os.path.join(self.tmp, "opt/homebrew/bin"), "ddc", target=cask + "/Contents/Helpers/deepdrivecleaner"
        )
        self.fails(self.install("--version", "0.8.0"), 4, path, "brew upgrade --cask deepdrivecleaner", "--force")
        self.assertFalse(os.path.exists(self.prefix))

    def test_a_cargo_install_is_named(self):
        self.publish("0.8.0")
        path = self.existing(os.path.join(self.home, ".cargo/bin"))
        self.fails(self.install("--version", "0.8.0"), 4, path, "cargo uninstall ddc-cli")
        self.assertFalse(os.path.exists(self.prefix))

    def test_the_app_is_named(self):
        self.publish("0.8.0")
        os.makedirs(self.app)
        self.fails(self.install("--version", "0.8.0"), 4, self.app, "brew upgrade --cask deepdrivecleaner")
        self.assertFalse(os.path.exists(self.prefix))

    def test_the_app_in_home_applications_is_named(self):
        self.publish("0.8.0")
        app = os.path.join(self.home, "Applications", "DeepDriveCleaner.app")
        os.makedirs(app)
        self.fails(self.install("--version", "0.8.0"), 4, app)
        self.assertFalse(os.path.exists(self.prefix))

    def test_a_folder_in_place_of_the_binary_is_refused(self):
        self.publish("0.8.0")
        os.makedirs(os.path.join(self.prefix, "deepdrivecleaner"))
        self.fails(self.install("--version", "0.8.0"), 1, "is a folder")
        self.assertEqual(os.listdir(os.path.join(self.prefix, "deepdrivecleaner")), [])
        self.assertEqual(self.release.archive_requests(), [], "nothing is downloaded")

    def test_another_copy_is_named_with_the_prefix_that_manages_it(self):
        self.publish("0.8.0")
        folder = os.path.join(self.home, "bin")
        path = self.existing(folder, "ddc")
        self.fails(self.install("--version", "0.8.0"), 4, path, f"--prefix {folder}")

    def test_force_installs_beside_another_copy(self):
        self.publish("0.8.0")
        self.existing(os.path.join(self.tmp, "opt/homebrew/bin"))
        os.makedirs(self.app)
        self.ok(self.install("--version", "0.8.0", "--force"))
        self.assertInstalled("0.8.0")

    def test_a_foreign_ddc_in_the_prefix_is_kept(self):
        self.publish("0.8.0")
        os.makedirs(self.prefix)
        with open(os.path.join(self.prefix, "ddc"), "w") as f:
            f.write("another tool\n")
        self.fails(self.install("--version", "0.8.0"), 4, "is not a link to")
        self.assertEqual(os.listdir(self.prefix), ["ddc"])

    def test_rosetta_installs_the_arm64_build(self):
        self.platform("Darwin", "x86_64", translated=1)
        self.publish("0.8.0")
        self.ok(self.install("--version", "0.8.0"))
        self.assertInstalled("0.8.0")

    def test_an_intel_mac_is_refused(self):
        self.platform("Darwin", "x86_64")
        self.fails(self.install("--version", "0.8.0"), 2, "Intel Macs are not supported")
        self.assertEqual(self.release.requests, [], "nothing is downloaded")

    def test_an_older_macos_is_refused(self):
        self.platform("Darwin", "arm64", macos="25.6")
        floor = constant(read("packaging/install/install.sh"), "MIN_MACOS")
        self.fails(self.install("--version", "0.8.0"), 2, f"macOS 25.6 is not supported", floor)

    def test_a_read_only_home_is_left_unchanged(self):
        # Root ignores the mode bits; the snapshot below still holds there.
        self.publish("0.8.0")
        os.makedirs(self.prefix)
        with open(os.path.join(self.home, ".zshrc"), "w") as f:
            f.write("# mine\n")

        def snapshot():
            seen = {}
            for dirpath, dirs, files in os.walk(self.tmp):
                for name in dirs + files:
                    p = os.path.join(dirpath, name)
                    if p.startswith((self.prefix, self.tmpdir)):
                        continue
                    st = os.lstat(p)
                    seen[p] = (st.st_mode, st.st_size, st.st_mtime_ns)
            return seen

        for d in (os.path.join(self.home, ".local"), self.home):
            os.chmod(d, 0o555)
        before = snapshot()
        self.ok(self.install("--version", "0.8.0"))
        self.assertEqual(snapshot(), before)
        self.assertInstalled("0.8.0")

    def test_modify_path_appends_one_line_once(self):
        self.publish("0.8.0")
        profile = os.path.join(self.home, ".zshrc")
        with open(profile, "w") as f:
            f.write("# mine, no newline at the end")
        self.ok(self.install("--version", "0.8.0", "--modify-path"))
        self.ok(self.install("--version", "0.8.0", "--modify-path"))
        with open(profile) as f:
            lines = f.read().splitlines()
        line = f'export PATH="{self.prefix}:$PATH" # added by the DeepDriveCleaner installer'
        self.assertEqual(lines, ["# mine, no newline at the end", line])
        bash = os.path.join(self.home, ".bash_profile")
        self.ok(self.install("--version", "0.8.0", "--modify-path", SHELL="/bin/bash"))
        with open(bash) as f:
            self.assertEqual(f.read(), line + "\n")

    def test_uninstall_removes_exactly_the_two_files(self):
        self.publish("0.8.0")
        self.ok(self.install("--version", "0.8.0"))
        with open(os.path.join(self.prefix, "mine"), "w") as f:
            f.write("kept\n")
        r = self.ok(self.install("--uninstall"))
        self.assertEqual(os.listdir(self.prefix), ["mine"])
        self.assertIn("removed", r.stdout)
        self.assertIn("nothing to remove", self.ok(self.install("--uninstall")).stdout)

    def test_uninstall_refuses_a_foreign_ddc(self):
        os.makedirs(self.prefix)
        for name in ("deepdrivecleaner", "ddc"):
            with open(os.path.join(self.prefix, name), "w") as f:
                f.write("x\n")
        self.fails(self.install("--uninstall"), 4, "did not create it")
        self.assertEqual(sorted(os.listdir(self.prefix)), ["ddc", "deepdrivecleaner"])

    def test_uninstall_stops_on_the_agent_that_runs_it(self):
        self.publish("0.8.0")
        self.ok(self.install("--version", "0.8.0"))
        agents = os.path.join(self.home, "Library", "LaunchAgents")
        os.makedirs(agents)
        plist = os.path.join(agents, f"{LABEL}.plist")
        dest = os.path.join(self.prefix, "deepdrivecleaner")
        with open(plist, "wb") as f:
            plistlib.dump({"Label": LABEL, "ProgramArguments": [dest, "scan", "--background"]}, f)
        self.fails(self.install("--uninstall"), 4, "ddc schedule uninstall")
        self.assertEqual(sorted(os.listdir(self.prefix)), ["ddc", "deepdrivecleaner"])
        # An agent of another copy (the cask's) does not stop it.
        with open(plist, "wb") as f:
            plistlib.dump({"Label": LABEL, "ProgramArguments": ["/opt/homebrew/bin/deepdrivecleaner"]}, f)
        self.ok(self.install("--uninstall"))
        self.assertEqual(os.listdir(self.prefix), [])


class LinuxInstall(Case):
    """The Linux half on any machine: uname says Linux, the stub is a shell script."""

    def setUp(self):
        super().setUp()
        self.platform("Linux", "x86_64")

    def test_before_the_linux_archives_it_is_not_released(self):
        self.release.publish("0.8.0", {DARWIN: archive(b"mac")})
        self.release.latest = "0.8.0"
        self.fails(self.install(), 2, "not released for Linux yet")
        self.assertFalse(os.path.exists(self.prefix))
        self.assertNothingLeft()

    def test_a_release_without_sums_is_not_released_for_linux(self):
        self.release.publish("0.7.0", {DARWIN: archive(b"mac")}, sums=False)
        self.fails(self.install("--version", "0.7.0"), 2, "not released for Linux yet")

    def test_the_musl_archive_installs(self):
        self.release.publish("0.9.0", {DARWIN: archive(b"mac"), MUSL: archive(sh_stub("0.9.0"))})
        self.ok(self.install("--version", "0.9.0"))
        self.assertInstalled("0.9.0")

    def test_aarch64_takes_its_own_archive(self):
        self.platform("Linux", "aarch64")
        self.release.publish("0.9.0", {MUSL: archive(sh_stub("0.9.0"))})
        self.fails(self.install("--version", "0.9.0"), 2, "not released for Linux yet")
        self.release.publish("0.9.1", {"aarch64-unknown-linux-musl": archive(sh_stub("0.9.1"))})
        self.ok(self.install("--version", "0.9.1"))
        self.assertInstalled("0.9.1")

    def test_a_tampered_musl_archive_installs_nothing(self):
        good = archive(sh_stub("0.9.0"))
        self.release.publish("0.9.0", {MUSL: good}, served={MUSL: archive(sh_stub("6.6.6"))})
        self.fails(self.install("--version", "0.9.0"), 3, "does not match SHA256SUMS")
        self.assertFalse(os.path.exists(self.prefix))
        self.assertNothingLeft()

    def test_an_unknown_architecture_is_refused(self):
        self.platform("Linux", "riscv64")
        self.fails(self.install("--version", "0.9.0"), 2, "unsupported Linux architecture riscv64")
        self.platform("FreeBSD", "amd64")
        self.fails(self.install("--version", "0.9.0"), 2, "unsupported system FreeBSD")
        self.assertEqual(self.release.requests, [])

    def test_upgrade_and_uninstall(self):
        self.release.publish("0.9.0", {MUSL: archive(sh_stub("0.9.0"))})
        self.release.publish("0.9.1", {MUSL: archive(sh_stub("0.9.1"))})
        self.ok(self.install("--version", "0.9.0"))
        r = self.ok(self.install("--version", "0.9.1", piped=True))
        self.assertIn("0.9.0 -> 0.9.1", r.stdout)
        self.assertInstalled("0.9.1")
        self.ok(self.install("--uninstall"))
        self.assertEqual(os.listdir(self.prefix), [])

    @unittest.skipUnless(LINUX, "the machine's own uname: Linux only")
    def test_on_this_linux_machine(self):
        os.remove(os.path.join(self.fakebin, "uname"))
        arch = os.uname().machine
        target = {"x86_64": MUSL, "aarch64": "aarch64-unknown-linux-musl"}[arch]
        self.release.publish("0.9.0-rc.1", {target: archive(sh_stub("0.9.0-rc.1"))})
        self.ok(self.install("--version", "0.9.0-rc.1"))
        self.assertInstalled("0.9.0-rc.1")


class Usage(Case):
    def test_bad_options_exit_2(self):
        for args in (["--bogus"], ["--version"], ["--version", "latest"], ["--prefix"], ["--uninstall", "--modify-path"]):
            with self.subTest(args=args):
                self.fails(self.install(*args), 2)
        self.assertEqual(self.release.requests, [])

    def test_help(self):
        r = self.ok(self.install("--help"))
        self.assertIn("--modify-path", r.stdout)

    def test_only_https_or_loopback_hosts(self):
        cases = [
            {"DDC_INSTALL_BASE_URL": "http://example.com"},
            {"DDC_INSTALL_BASE_URL": "http://127.0.0.1.evil.com:80"},
            {"DDC_INSTALL_BASE_URL": "http://user@127.0.0.1:80"},
            {"DDC_INSTALL_BASE_URL": "http://127.0.0.1:80@evil.com"},
            {"DDC_INSTALL_BASE_URL": "http://127.0.0.1:x/"},
            {"DDC_INSTALL_BASE_URL": "file:///tmp"},
            {"DDC_INSTALL_BASE_URL": "https://example.com"},  # https, but with ALLOW_ADHOC
            {"DDC_INSTALL_BASE_URL": None},
            {"DDC_INSTALL_ALLOW_ADHOC": None},  # DDC_INSTALL_APP_PATH alone
        ]
        for env in cases:
            with self.subTest(env=env):
                self.fails(self.install("--version", "0.8.0", **env), 2, "DDC_INSTALL_")
        self.assertEqual(self.release.requests, [])

    def test_a_truncated_download_runs_nothing(self):
        with open(SCRIPT) as f:
            text = f.read()
        self.assertTrue(text.endswith('\nmain "$@"\n'), "main runs on the last line")
        for cut in (len(text) // 3, len(text) // 2, len(text) - len('main "$@"\n') - 1, len(text) - 2):
            with self.subTest(cut=cut):
                r = subprocess.run(
                    [SHELL, "-s", "--", "--version", "0.8.0"], input=text[:cut], env=self.env(),
                    capture_output=True, text=True, timeout=TIMEOUT,
                )
                self.assertEqual(r.stdout, "")
                self.assertNotIn("Installing", r.stdout + r.stderr)
        self.assertEqual(self.release.requests, [])
        self.assertFalse(os.path.exists(self.prefix))

    def test_the_script_never_calls_sudo(self):
        with open(SCRIPT) as f:
            code = [line for line in f if not line.lstrip().startswith("#")]
        self.assertFalse([line for line in code if re.search(r"\bsudo\b", line)])


class Constants(unittest.TestCase):
    """install.sh carries the release's constants; they agree with where they come from."""

    def test_the_constants_agree(self):
        script = read("packaging/install/install.sh")
        publish = read("scripts/publish-release.sh")
        dist = read("scripts/build-dist.sh")
        app = read("scripts/build-app.sh")
        cli = read("scripts/build-cli.sh")
        team = constant(script, "TEAM_ID")
        self.assertRegex(team, r"^[A-Z0-9]{10}$")
        for text in (publish, dist, app):
            self.assertEqual(constant(text, "TEAM_ID"), team)
        cli_id = constant(script, "CLI_ID")
        self.assertEqual(constant(dist, "IDENTIFIER"), cli_id)
        self.assertEqual(constant(publish, "CLI_ID"), cli_id)
        self.assertEqual(constant(app, "CLI_ID"), cli_id)
        floor = constant(script, "MIN_MACOS")
        self.assertEqual(constant(publish, "MIN_MACOS"), floor)
        for name, text in (("build-cli.sh", cli), ("build-app.sh", app)):
            self.assertEqual(re.findall(r"MACOSX_DEPLOYMENT_TARGET=([0-9.]+)", text), [floor], name)
        project = read("app/DeepDriveCleaner.xcodeproj/project.pbxproj")
        self.assertEqual(set(re.findall(r"MACOSX_DEPLOYMENT_TARGET = ([0-9.]+);", project)), {floor})
        launchd = read("crates/ddc-cli/src/launchd.rs")
        self.assertIn(f'pub const LABEL: &str = "{constant(script, "LABEL")}";', launchd)
        self.assertIn('ARCHIVE=deepdrivecleaner-$VERSION-$TARGET.tar.gz', script)
        self.assertIn('ASSET="deepdrivecleaner-$VERSION-$TARGET.tar.gz"', dist)
        self.assertIn(
            '"{ repo }/releases/download/v{ version }/deepdrivecleaner-{ version }-{ target }.tar.gz"',
            read("crates/ddc-cli/Cargo.toml"),
        )


@unittest.skipUnless(REAL_AGENT, "creates a real LaunchAgent: the Install workflow's macOS runner only")
class RealAgent(Case):
    """`ddc schedule install` of the real latest release, then an upgrade by the script:
    the agent launchd holds still runs the binary at the same path."""

    def test_the_agent_survives_an_upgrade(self):
        repo = "https://github.com/SmartAndPoint/DeepDriveCleaner/releases"
        with urllib.request.urlopen(f"{repo}/latest") as r:
            real = r.geturl().rsplit("/v", 1)[1]
        with urllib.request.urlopen(f"{repo}/download/v{real}/deepdrivecleaner-{real}-{DARWIN}.tar.gz") as r:
            real_archive = r.read()
        self.release.publish(real, {DARWIN: real_archive})
        self.release.publish("99.0.0", {DARWIN: archive(mac_stub("99.0.0"))})
        uid = os.getuid()
        target = f"gui/{uid}/{LABEL}"
        plist = os.path.join(self.home, "Library", "LaunchAgents", f"{LABEL}.plist")
        self.addCleanup(lambda: subprocess.run(["launchctl", "bootout", target], capture_output=True))
        self.addCleanup(lambda: os.path.exists(plist) and os.remove(plist))
        self.ok(self.install("--version", real))
        dest = os.path.join(self.prefix, "deepdrivecleaner")
        env = {"PATH": "/usr/bin:/bin", "HOME": self.home}
        out = subprocess.run([dest, "schedule", "install"], env=env, capture_output=True, text=True, timeout=60)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)

        self.ok(self.install("--version", "99.0.0"))

        with open(plist, "rb") as f:
            agent = plistlib.load(f)
        self.assertEqual(agent["ProgramArguments"][0], dest)
        log = agent["StandardOutPath"]
        subprocess.run(["launchctl", "kickstart", "-k", target], check=True, capture_output=True)
        # The stub prints "deepdrivecleaner 99.0.0 ran" for scan --background: in the
        # agent's log, it is the new binary that launchd ran.
        deadline = time.time() + 30
        while True:
            printed = subprocess.run(["launchctl", "print", target], capture_output=True, text=True).stdout
            ran = ""
            if os.path.exists(log):
                with open(log) as f:
                    ran = f.read()
            if ("last exit code = 0" in printed and "deepdrivecleaner 99.0.0 ran" in ran) or time.time() > deadline:
                break
            time.sleep(1)
        self.assertIn(dest, printed)
        self.assertIn("last exit code = 0", printed)
        self.assertIn("deepdrivecleaner 99.0.0 ran\n", ran)
        # The script's own uninstall stops on it.
        self.fails(self.install("--uninstall"), 4, "ddc schedule uninstall")


class Concurrent(unittest.TestSuite):
    """Every case at once: each has its own folders and server, and most of a case's
    time is macOS checking each freshly written executable on its first run."""

    def run(self, result, debug=False):
        workers = min(4, os.cpu_count() or 1)
        with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
            for _ in pool.map(lambda test: test(result), self):
                pass
        return result


def load_tests(loader, standard, pattern):
    def cases(suite):
        for test in suite:
            yield from cases(test) if isinstance(test, unittest.TestSuite) else [test]

    return Concurrent(cases(standard))


if __name__ == "__main__":
    unittest.main()
