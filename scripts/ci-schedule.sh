#!/bin/bash
# The Linux scheduled scan on a real systemd user manager (story "Scheduled scans on
# Linux through a systemd user timer", DDC-010):
#
#   scripts/ci-schedule.sh <deepdrivecleaner>     on the Gate's linux-schedule job
#
# Installs the timer with a throwaway data folder, forces one run of the service, checks
# what it wrote, reinstalls weekly and uninstalls twice. CI only: it turns on lingering for
# the runner's user and runs a real `ddc schedule install` into that user's manager, which
# only a runner thrown away after the job can take. The service runs with the runner's own
# HOME (ddc never pins HOME), so the forced run scans the runner's home. The uninstall also
# runs on any exit, so a failed step leaves no timer.
set -euo pipefail
export LC_ALL=C

fail() { printf 'ci-schedule: %s\n' "$1" >&2; exit 1; }

[ "$#" -eq 1 ] || { echo "usage: scripts/ci-schedule.sh <deepdrivecleaner>" >&2; exit 2; }
[ "$(uname -s)" = Linux ] || fail "Linux only"
[ "${CI:-}" = true ] || fail "this installs a real systemd user timer and turns on lingering: CI runners only"
sudo -n true 2>/dev/null || fail "sudo asks for a password here; nothing was installed"

# A stable copy: ddc refuses to schedule a binary inside cargo's target folder.
stable="$RUNNER_TEMP/stable"
mkdir -p "$stable"
cp "$1" "$stable/deepdrivecleaner"
ddc="$stable/deepdrivecleaner"
data="$RUNNER_TEMP/ddc-schedule-data"
export DDC_DATA_DIR="$data"

# The runner's user has no session: lingering starts its manager; the step sets no
# runtime folder. `systemctl --user` reaches the manager through its private socket,
# so the session bus (dbus-user-session) is not needed.
sudo -n loginctl enable-linger "$USER"
uid="$(id -u)"
export XDG_RUNTIME_DIR="/run/user/$uid"
for _ in $(seq 1 30); do
    [ -S "$XDG_RUNTIME_DIR/systemd/private" ] && break
    sleep 1
done
[ -S "$XDG_RUNTIME_DIR/systemd/private" ] || fail "the user manager did not come up"
timeout 60 systemctl --user is-system-running --wait || true

cleanup() { "$ddc" schedule uninstall || true; }
trap cleanup EXIT

# Every output goes to a file before grep reads it: `grep -q` stops reading at its
# first match, and ddc would then die writing to a closed pipe.
out="$RUNNER_TEMP/schedule-out.txt"
"$ddc" schedule install --interval daily
"$ddc" schedule status > "$out"
cat "$out"
grep -q "^Enabled: yes" "$out" || fail "the timer is not enabled"
grep -q "^Next:    [A-Z0-9]" "$out" || fail "no next run"
grep -q "^Data:    $data\$" "$out" || fail "status does not name the pinned data folder"

# One forced run, as the timer would start it.
systemctl --user start deepdrivecleaner-scan.service
[ -f "$data/last-scan.json" ] || fail "the forced run wrote no $data/last-scan.json"
[ -f "$data/logs/background.log" ] || fail "the forced run wrote no $data/logs/background.log"
# Without DDC_DATA_DIR in this shell, status still reads the scan of the pinned folder.
env -u DDC_DATA_DIR "$ddc" schedule status > "$out"
cat "$out"
grep -q "^Result:  success" "$out" || fail "the forced run did not succeed"
if ! grep -q "^Last scan: " "$out" || grep -q "^Last scan: none yet" "$out"; then
    fail "status did not read the last scan from the pinned data folder"
fi

"$ddc" schedule install --interval weekly
units="$HOME/.local/state/deepdrivecleaner/systemd"
grep -q "^OnCalendar=weekly$" "$units/deepdrivecleaner-scan.timer" || fail "the weekly install kept the old calendar"

"$ddc" schedule uninstall > "$out"
grep -q "^Removed the background scan.$" "$out" || fail "uninstall removed nothing"
[ ! -e "$units/deepdrivecleaner-scan.service" ] && [ ! -e "$units/deepdrivecleaner-scan.timer" ] \
    || fail "uninstall left a unit file"
"$ddc" schedule uninstall > "$out"
grep -q "^No background scan was installed.$" "$out" || fail "a second uninstall was not a no-op"
echo "ci-schedule: install, status, a forced run, weekly and uninstall all passed"
