"""Build the coverage baseline for epic DDC-001 (read-only).

Input:  bench/devartifacts-2026-10-17.txt  (candidate artifact dirs from `find $HOME -xdev -prune`, '~'-relative)
Output: TSV on stdout: path, bytes (du -skx * 1024), rule id, expected class.
Rule ids and path granularity follow the catalog in story "Detection rule catalog".
Run from a terminal with Full Disk Access:  python3 -I bench/make_baseline.py > bench/baseline-YYYY-MM-DD.tsv
"""
import glob
import os
import subprocess
import sys

H = os.path.expanduser("~")
HERE = os.path.dirname(os.path.abspath(__file__))
LOCKS_PY = ["uv.lock", "poetry.lock", "Pipfile.lock"]
LOCKS_JS = ["package-lock.json", "pnpm-lock.yaml", "yarn.lock", "bun.lock", "bun.lockb"]


GLOBAL_NODE = ("/.npm-global/", "/.config/yarn/global/", "/.bun/install/global/", "/.volta/", "/.fnm/")

# Sealed like an app bundle (ADR "Agent footprint: worktrees, harness data and bounded consent for agents", §3):
# never a finding, and no rule fires inside. The worktree folders are not sealed: the rules work inside a worktree.
SEALED = (".claude/sessions", ".claude/shell-snapshots", ".claude/plugins",
          ".cursor/extensions", ".cursor/plugins", ".cursor/plans", ".cursor/chats")


def du(p):
    """(allocated bytes, note): note is '' (complete), 'partial' (some subpaths unreadable) or 'unreadable' (bytes = -1)."""
    r = subprocess.run(["du", "-skx", p], capture_output=True, text=True)
    out = r.stdout.split()
    if not out:
        return -1, "unreadable"
    return int(out[0]) * 1024, "" if r.returncode == 0 else "partial"


def app_owned(p):
    """Environments under a hidden top-level dir of $HOME or under ~/Library belong to apps/tools."""
    rel = os.path.relpath(p, H)
    return rel.startswith(".") or rel.startswith("Library" + os.sep)


def project_root(p):
    cur = os.path.dirname(p)
    while cur.startswith(H) and cur != H:
        if os.path.exists(os.path.join(cur, ".git")):
            return cur
        cur = os.path.dirname(cur)
    return os.path.dirname(p)


def has_lock(p, locks, requirements=False):
    for d in {os.path.dirname(p), project_root(p)}:
        if any(os.path.exists(os.path.join(d, name)) for name in locks):
            return True
        if requirements and glob.glob(os.path.join(d, "requirements*.txt")):
            return True
    return False


def classify(p):
    n, parent = os.path.basename(p), os.path.dirname(p)
    if os.path.isfile(os.path.join(p, "pyvenv.cfg")):
        ok = has_lock(p, LOCKS_PY, True) and not app_owned(p)
        return "python.venv", "rebuildable" if ok else "review"
    if n == "node_modules":
        if "/.nvm/versions/node/" in p or any(g in p + "/" for g in GLOBAL_NODE):
            return None  # global packages are not findings
        if os.path.isfile(os.path.join(parent, "package.json")):
            ok = has_lock(p, LOCKS_JS) and not app_owned(p)
            return "node.node_modules", "rebuildable" if ok else "review"
        return "node.node_modules-orphan", "review"
    if n == "target" and os.path.isfile(os.path.join(parent, "Cargo.toml")):
        return "rust.target", "rebuildable"
    if n == "target" and os.path.isfile(os.path.join(parent, "pom.xml")):
        return "maven.target", "rebuildable"
    if n in (".next", ".nuxt", ".svelte-kit", ".turbo", ".parcel-cache", ".angular") and os.path.isfile(os.path.join(parent, "package.json")):
        return "js.build-cache", "safe"
    conventional = n.startswith("build") or n.startswith("cmake-build-") or n in ("out", "_build", ".build")
    if conventional and os.path.isfile(os.path.join(p, "CMakeCache.txt")) and not os.path.isfile(os.path.join(p, "CMakeLists.txt")):
        home_dir = next((l.split("=", 1)[1].strip() for l in read_first(os.path.join(p, "CMakeCache.txt")).splitlines()
                         if l.startswith("CMAKE_HOME_DIRECTORY:")), "")
        src = os.path.realpath(home_dir) if home_dir and os.path.exists(home_dir) else None
        if src and not (src + os.sep).startswith(os.path.realpath(p) + os.sep):
            return "cmake.build", "rebuildable"
    if n == "build" and glob.glob(os.path.join(parent, "build.gradle*")) and any(
        os.path.isdir(os.path.join(p, x)) for x in ("intermediates", "classes", "tmp")
    ):
        return "gradle.build", "rebuildable"
    if n == "Pods" and os.path.isfile(os.path.join(parent, "Podfile")):
        return "cocoapods.pods", "rebuildable" if os.path.isfile(os.path.join(parent, "Podfile.lock")) else "review"
    return None  # __pycache__ (thousands of tiny dirs) and ambiguous build/dist are not per-path baseline rows


def read_first(path):
    try:
        with open(path) as f:
            return f.read().strip()
    except OSError:
        return ""


def known():
    rows = [
        (".npm/_cacache", "cache.npm", "safe"), (".npm/_npx", "cache.npm", "safe"),
        ("Library/Caches/Yarn", "cache.yarn", "safe"), (".yarn/berry/cache", "cache.yarn", "safe"),
        (".bun/install/cache", "cache.bun", "safe"),
        ("Library/pnpm/store", "cache.pnpm", "safe"), (".pnpm-store", "cache.pnpm", "safe"),
        (".cache/uv", "cache.uv", "safe"), ("Library/Caches/pip", "cache.pip", "safe"),
        ("Library/Caches/pypoetry/cache", "cache.poetry", "safe"), ("Library/Caches/pypoetry/artifacts", "cache.poetry", "safe"),
        ("Library/Caches/Homebrew", "cache.homebrew", "safe"),
        ("Library/Caches/go-build", "cache.go-build", "safe"), ("go/pkg/mod", "cache.go-mod", "safe"),
        (".cargo/registry/cache", "cache.cargo", "safe"), (".cargo/registry/src", "cache.cargo", "safe"),
        (".cargo/git/checkouts", "cache.cargo", "safe"),
        (".gradle/caches", "cache.gradle", "safe"), (".m2/repository", "repo.maven", "review"),
        ("Library/Caches/CocoaPods", "cache.cocoapods", "safe"),
        ("Library/Caches/ms-playwright", "browsers.playwright", "review"),
        (".cache/puppeteer", "browsers.puppeteer", "review"), ("Library/Caches/Cypress", "browsers.cypress", "review"),
        ("Library/Developer/Xcode/DerivedData", "xcode.derived-data", "safe"),
        ("Library/Developer/CoreSimulator/Caches", "xcode.simulator-caches", "safe"),
        ("Library/Developer/Xcode/iOS DeviceSupport", "xcode.device-support", "review"),
        ("Library/Developer/Xcode/Archives", "xcode.archives", "user-data"),
        ("Library/Containers/com.docker.docker", "vm.docker-desktop", "review"),
        (".local/share/containers", "vm.podman", "review"), (".colima", "vm.colima", "review"),
        (".lima", "vm.lima", "review"), (".orbstack", "vm.orbstack", "review"),
        ("Library/Application Support/rancher-desktop", "vm.rancher-desktop", "review"),
        ("Library/Caches/colima", "cache.colima", "safe"),
        (".ollama/models", "model.ollama", "review"), (".cache/huggingface", "model.huggingface", "review"),
        (".lmstudio/models", "model.lmstudio", "review"), (".cache/torch", "model.torch", "review"),
        ("Library/Caches/JetBrains", "ide.jetbrains-cache", "safe"),
        ("Library/Caches/Google/Chrome", "app.chrome-cache", "safe"),
        ("Library/Caches/BraveSoftware", "app.brave-cache", "safe"),
        ("Library/Caches/Firefox", "app.firefox-cache", "safe"),
        ("Library/Caches/com.spotify.client", "app.spotify-cache", "safe"),
        ("Library/Application Support/Slack/Cache", "app.slack-cache", "safe"),
        ("Library/Application Support/Slack/Service Worker/CacheStorage", "app.slack-cache", "safe"),
        ("Library/Caches/Adobe Camera Raw 2", "app.adobe-camera-raw-cache", "safe"),
        ("Pictures", "user.pictures", "user-data"), ("Movies", "user.movies", "user-data"),
        ("Music", "user.music", "user-data"), ("Downloads", "user.downloads", "user-data"),
        ("Library/Application Support/MobileSync/Backup", "user.ios-backups", "user-data"),
        ("Library/Application Support/iMazing/Backups", "user.imazing-backups", "user-data"),
        ("Library/Group Containers/6N38VWS5BX.ru.keepcoder.Telegram", "user.telegram", "user-data"),
        ("Library/Group Containers/group.net.whatsapp.WhatsApp.shared", "user.whatsapp", "user-data"),
    ]
    rows += agent_rows()
    for app, rule in (("Code", "ide.vscode-cache"), ("Cursor", "ide.cursor-cache"), ("Windsurf", "ide.windsurf-cache")):
        for c in ("Cache", "CachedData", "CachedExtensionVSIXs", "GPUCache", "logs"):
            rows.append((f"Library/Application Support/{app}/{c}", rule, "safe"))
        rows.append((f"Library/Application Support/{app}/User/workspaceStorage", "ide.workspace-storage", "user-data"))
    pinned = project_pins()
    nvm = sorted(glob.glob(H + "/.nvm/versions/node/*"), key=lambda d: version_key(os.path.basename(d)))
    keep = nvm_protected([os.path.basename(d) for d in nvm], pinned["node"])
    for d in nvm:
        if keep is not None and os.path.basename(d) not in keep:
            rows.append((os.path.relpath(d, H), "toolchain.nvm", "review"))
    py = sorted(glob.glob(H + "/.pyenv/versions/*"))
    py_keep = set(read_first(os.path.join(H, ".pyenv/version")).split()) | pinned["python"]
    if len(py) > 1:
        for d in py:
            if os.path.basename(d) not in py_keep:
                rows.append((os.path.relpath(d, H), "toolchain.pyenv", "review"))
    settings = read_first(os.path.join(H, ".rustup/settings.toml"))
    default_tc = next((l.split("=", 1)[1].strip().strip('"') for l in settings.splitlines() if l.startswith("default_toolchain")), "")
    tcs = sorted(glob.glob(H + "/.rustup/toolchains/*"))
    if len(tcs) > 1:
        for d in tcs:
            name = os.path.basename(d)
            if not any(name.startswith(t) for t in {default_tc, *pinned["rust"]} if t):
                rows.append((os.path.relpath(d, H), "toolchain.rustup", "review"))
    return rows


def agent_rows():
    """The coding agents' rules: harness scratch is safe, the rest review. One row per Claude Code project folder."""
    rows = [(f".claude/{d}", "agent.claude-code-scratch", "safe") for d in ("debug", "todos", "statsig", "logs")]
    rows += [
        (".claude/file-history", "agent.claude-code-file-history", "review"),
        (".claude/paste-cache", "agent.claude-code-history", "review"),
        (".claude/usage-data", "agent.claude-code-history", "review"),
        (".codex/sessions", "agent.codex-sessions", "review"), (".codex/log", "agent.codex-sessions", "review"),
        (".cache/codex-runtimes", "agent.codex-runtimes", "review"),
        (".cache/opencode", "agent.opencode-data", "review"),
        (".cursor/projects", "agent.cursor-agent-data", "review"),
        (".cursor/ai-tracking", "agent.cursor-agent-data", "review"),
    ]
    # By subfolder, never whole: the folder also holds auth.json and opencode.db.
    rows += [(f".local/share/opencode/{d}", "agent.opencode-data", "review") for d in ("storage", "snapshot", "log")]
    for d in sorted(glob.glob(glob.escape(H) + "/.claude/projects/*")):
        if os.path.isdir(d) and not os.path.islink(d):
            rows.append((os.path.relpath(d, H), "agent.claude-code-transcripts", "review"))
    return rows


def version_key(v):
    return tuple(int(x) if x.isdigit() else 0 for x in v.lstrip("v").split("."))


def nvm_resolve(alias, installed, depth=0):
    """Resolve an nvm alias to installed version names; None = unresolvable (rule yields nothing)."""
    alias = alias.strip()
    if not alias or alias == "system" or depth > 2:
        return None
    if alias in ("node", "stable"):
        return {installed[-1]} if installed else None
    if alias.startswith("lts/"):
        target = read_first(os.path.join(H, ".nvm/alias", alias))
        return nvm_resolve(target, installed, depth + 1)
    prefix = alias.lstrip("v")
    hits = [v for v in installed if v.lstrip("v") == prefix or v.lstrip("v").startswith(prefix + ".")]
    return {hits[-1]} if hits else None


def nvm_protected(installed, pinned):
    if len(installed) <= 1:
        return set(installed)
    default = nvm_resolve(read_first(os.path.join(H, ".nvm/alias/default")), installed)
    if default is None:
        return None
    keep = set(default)
    for pin in pinned:
        keep |= nvm_resolve(pin, installed) or set()
    return keep


def project_pins():
    """Versions pinned by project files under ~/Projects (.nvmrc, .node-version, .python-version, rust-toolchain*)."""
    pins = {"node": set(), "python": set(), "rust": set()}
    root = os.path.join(H, "Projects")
    if not os.path.isdir(root):
        return pins
    out = subprocess.run(
        ["find", root, "-xdev", "(", "-name", "node_modules", "-o", "-name", ".venv", "-o", "-name", ".git", ")", "-prune",
         "-o", "(", "-name", ".nvmrc", "-o", "-name", ".node-version", "-o", "-name", ".python-version",
         "-o", "-name", "rust-toolchain", "-o", "-name", "rust-toolchain.toml", ")", "-type", "f", "-print"],
        capture_output=True, text=True).stdout.splitlines()
    for f in out:
        text = read_first(f)
        name = os.path.basename(f)
        if name in (".nvmrc", ".node-version"):
            pins["node"].add(text.split()[0] if text else "")
        elif name == ".python-version":
            pins["python"].update(text.split())
        else:
            for line in text.splitlines():
                if line.strip().startswith("channel"):
                    pins["rust"].add(line.split("=", 1)[1].strip().strip('"'))
                elif name == "rust-toolchain" and line.strip():
                    pins["rust"].add(line.strip())
    return pins


def main():
    rows = []
    known_rows = known()
    # Artifacts inside a known path (e.g. node_modules in ~/.npm/_npx) belong to that finding.
    # Regions (Downloads) keep rules working inside, so they do not swallow artifacts.
    known_roots = [os.path.join(H, rel) + os.sep for rel, rule, _ in known_rows if rule != "user.downloads"]
    known_roots += [os.path.join(H, rel) + os.sep for rel in SEALED]
    with open(os.path.join(HERE, "devartifacts-2026-10-17.txt")) as f:
        for line in f:
            p = line.rstrip("\n").replace("~", H, 1)
            if any(p.startswith(k) for k in known_roots) or ".app/" in p:
                continue  # inside a known path, a sealed harness folder or a sealed .app bundle
            if os.path.isdir(p) and not os.path.islink(p):
                hit = classify(p)
                if hit:
                    rule, cls = hit
                    if cls == "rebuildable" and app_owned(p):
                        cls = "review"  # project artifacts owned by apps/tools (hidden top-level dirs, ~/Library)
                    rows.append((p, rule, cls))
    for rel, rule, cls in known_rows:
        p = os.path.join(H, rel)
        if os.path.isdir(p) and not os.path.islink(p):
            rows.append((p, rule, cls))
    out = sys.stdout
    fda = os.access(os.path.join(H, "Library/Application Support/com.apple.TCC/TCC.db"), os.R_OK)
    out.write("# DDC-001 coverage baseline · reference machine · bytes = du -skx * 1024 at generation time; "
              f"note: partial/unreadable = du could not read everything; full_disk_access={'yes' if fda else 'no'}\n")
    out.write("path\tbytes\trule\tclass\tnote\n")
    total = 0
    for p, rule, cls in rows:
        b, note = du(p)
        total += max(b, 0)
        out.write(f"{p.replace(H, '~', 1)}\t{b}\t{rule}\t{cls}\t{note}\n")
    sys.stderr.write(f"rows={len(rows)} total={total / 2**30:.1f} GiB\n")


if __name__ == "__main__":
    main()
