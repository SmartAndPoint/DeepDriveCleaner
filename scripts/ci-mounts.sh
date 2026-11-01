#!/bin/bash
# Makes the filesystems the Linux safety tests need (story "Linux CI mounts: a filesystem
# matrix job with root-only steps", DDC-010) and prints where each one is:
#
#   scripts/ci-mounts.sh up <base>     on the Gate's linux-mounts job: >> "$GITHUB_ENV"
#
# Every mount and fixture lies under <base>, which must be outside $HOME (the test trees'
# guard refuses a tree inside it). Each mount's top belongs to the user who runs this,
# except the tmpfs made to be root-owned. Prints one KEY=value line per mount and fixture
# (tests/common/mounts.rs reads them), and a table of each mount's filesystem type to
# stderr and to $GITHUB_STEP_SUMMARY when it is set. Every privileged step goes through
# `sudo -n`, which fails instead of asking. CI only: it edits /etc/exports and Samba's
# configuration, starts their servers and makes a zpool, with nothing to undo it, which
# only a runner thrown away after the job can take.
set -euo pipefail
export LC_ALL=C

fail() { printf 'ci-mounts: %s\n' "$1" >&2; exit 1; }
usage() { echo "usage: scripts/ci-mounts.sh up <base>" >&2; exit 2; }

[ "${1:-}" = up ] || usage
shift
[ "$#" -eq 1 ] || usage
[ "$(uname -s)" = Linux ] || fail "Linux only"
[ "${CI:-}" = true ] || fail "this changes the machine's NFS, Samba and ZFS setup for good: CI runners only"
sudo -n true 2>/dev/null || fail "sudo asks for a password here; nothing was mounted"

parent="$(cd "$(dirname "$1")" && pwd -P)" || fail "no folder above $1"
base="$parent/$(basename "$1")"
home="$(cd "$HOME" && pwd -P)"
case "$base/" in "$home"/*) fail "$base is inside \$HOME, where the test trees' guard refuses them" ;; esac
mkdir -p "$base"
[ -z "$(ls -A "$base")" ] || fail "$base is not empty"

uid="$(id -u)"
gid="$(id -g)"
img="$base/img"
mkdir -p "$img"

say() { printf '%s=%s\n' "$1" "$2"; }
mine() { sudo -n chown "$uid:$gid" "$1"; }

# A sparse image of $2 MiB with a filesystem made by "$3 …", loop-mounted at $base/$1.
loop() {
    local name="$1" mib="$2"
    shift 2
    truncate -s "${mib}M" "$img/$name.img"
    "$@" "$img/$name.img" >/dev/null
    mkdir -p "$base/$name"
    sudo -n mount -o loop "$img/$name.img" "$base/$name"
    mine "$base/$name"
}

packages=(btrfs-progs xfsprogs zfsutils-linux nfs-kernel-server nfs-common samba cifs-utils exfatprogs e2fsprogs autofs)
sudo -n apt-get update -qq >&2 || { sleep 10; sudo -n apt-get update -qq >&2; }
sudo -n env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${packages[@]}" >/dev/null

# btrfs: a subvolume and two snapshots of it, read-only and writable, all made by the
# user, as a developer makes them.
loop btrfs 300 mkfs.btrfs -q
btrfs subvolume create "$base/btrfs/subvolume" >/dev/null
echo data >"$base/btrfs/subvolume/file"
btrfs subvolume snapshot -r "$base/btrfs/subvolume" "$base/btrfs/snapshot-ro" >/dev/null
btrfs subvolume snapshot "$base/btrfs/subvolume" "$base/btrfs/snapshot-rw" >/dev/null
say DDC_MOUNT_BTRFS "$base/btrfs"
say DDC_MOUNT_BTRFS_SUBVOLUME "$base/btrfs/subvolume"
say DDC_MOUNT_BTRFS_SNAPSHOT_RO "$base/btrfs/snapshot-ro"
say DDC_MOUNT_BTRFS_SNAPSHOT_RW "$base/btrfs/snapshot-rw"

# XFS refuses a filesystem under 300 MB; reflinks are on by default.
loop xfs 320 mkfs.xfs -q
say DDC_MOUNT_XFS "$base/xfs"

# ext4 with casefolding, and one folder that folds case (chattr +F, the user's own).
loop casefold 64 mkfs.ext4 -q -O casefold
mkdir "$base/casefold/folded"
chattr +F "$base/casefold/folded"
say DDC_MOUNT_CASEFOLD "$base/casefold/folded"

# exFAT has no owners: the mount gives every file to the user.
truncate -s 64M "$img/exfat.img"
mkfs.exfat "$img/exfat.img" >/dev/null
mkdir -p "$base/exfat"
sudo -n mount -o "loop,uid=$uid,gid=$gid" "$img/exfat.img" "$base/exfat"
say DDC_MOUNT_EXFAT "$base/exfat"

# tmpfs twice: the user's own, and one whose top is root's with a folder of the user's
# inside, where no .Trash-$uid can be made at the top.
mkdir -p "$base/tmpfs" "$base/tmpfs-root-top"
sudo -n mount -t tmpfs -o "size=64m,uid=$uid,gid=$gid,mode=0755" tmpfs "$base/tmpfs"
sudo -n mount -t tmpfs -o size=64m,mode=0755 tmpfs "$base/tmpfs-root-top"
sudo -n install -d -o "$uid" -g "$gid" "$base/tmpfs-root-top/user"
say DDC_MOUNT_TMPFS "$base/tmpfs"
say DDC_MOUNT_TMPFS_ROOT_TOP "$base/tmpfs-root-top/user"

# ZFS: a pool on a file, a child dataset and a snapshot of it (`.zfs/snapshot/ddc`).
truncate -s 300M "$img/zfs.img"
sudo -n modprobe zfs
sudo -n zpool create -m "$base/zfs" ddcmounts "$img/zfs.img"
sudo -n zfs create ddcmounts/child
mine "$base/zfs"
mine "$base/zfs/child"
echo data >"$base/zfs/child/file"
sudo -n zfs snapshot ddcmounts/child@ddc
say DDC_MOUNT_ZFS "$base/zfs"
say DDC_MOUNT_ZFS_CHILD "$base/zfs/child"

# NFS on loopback: an export as the user, and one with all_squash, where every file the
# user writes belongs to nobody.
mkdir -p "$base/export/plain" "$base/export/squash" "$base/nfs" "$base/nfs-squash"
sudo -n chown 65534:65534 "$base/export/squash"
sudo -n chmod 0777 "$base/export/squash"
{
    echo "$base/export/plain 127.0.0.1(rw,sync,no_subtree_check,insecure)"
    echo "$base/export/squash 127.0.0.1(rw,sync,no_subtree_check,insecure,all_squash,anonuid=65534,anongid=65534)"
} | sudo -n tee -a /etc/exports >/dev/null
sudo -n systemctl start nfs-kernel-server
sudo -n exportfs -ra
sudo -n mount -t nfs -o vers=4.2 "127.0.0.1:$base/export/plain" "$base/nfs"
sudo -n mount -t nfs -o vers=4.2 "127.0.0.1:$base/export/squash" "$base/nfs-squash"
say DDC_MOUNT_NFS "$base/nfs"
say DDC_MOUNT_NFS_SQUASH "$base/nfs-squash"

# CIFS on loopback through Samba, as a guest mapped to the user.
mkdir -p "$base/export/cifs" "$base/cifs"
printf '[ddcmounts]\n  path = %s\n  read only = no\n  guest ok = yes\n  force user = %s\n  hosts allow = 127.0.0.1\n  hosts deny = ALL\n' \
    "$base/export/cifs" "$(id -un)" | sudo -n tee -a /etc/samba/smb.conf >/dev/null
sudo -n systemctl restart smbd
sudo -n mount -t cifs //127.0.0.1/ddcmounts "$base/cifs" -o "guest,uid=$uid,gid=$gid,vers=3.0"
say DDC_MOUNT_CIFS "$base/cifs"

# A bind mount of this filesystem inside a test root: the same device on both sides.
mkdir -p "$base/bind-source" "$base/bind-root/inner"
echo data >"$base/bind-source/file"
sudo -n mount --bind "$base/bind-source" "$base/bind-root/inner"
say DDC_MOUNT_BIND "$base/bind-root"

# An autofs direct map: its trigger is an autofs mount that binds a folder of this
# filesystem when opened. stat -f and statfs(2) trigger it, so it stays out of the
# table below, and the tests tell it by /proc/self/mountinfo (story "Linux mount
# boundaries: automounts, bind mounts and the Trash across filesystems", DDC-010).
mkdir -p "$base/autofs-source" "$base/autofs"
echo data >"$base/autofs-source/file"
echo "/- /etc/auto.ddcmounts" | sudo -n tee -a /etc/auto.master >/dev/null
echo "$base/autofs/trigger -fstype=bind :$base/autofs-source" | sudo -n tee /etc/auto.ddcmounts >/dev/null
sudo -n systemctl restart autofs
for _ in $(seq 50); do
    awk -v p="$base/autofs/trigger" '$5 == p && / - autofs /' /proc/self/mountinfo | grep -q . && break
    sleep 0.2
done
awk -v p="$base/autofs/trigger" '$5 == p && / - autofs /' /proc/self/mountinfo | grep -q . || fail "autofs did not set up $base/autofs/trigger"
say DDC_MOUNT_AUTOFS "$base/autofs/trigger"

# Files only root can make, each in a folder of its own, so a test that breaks one
# breaks no other: a device node, an immutable file, an append-only file.
mkdir -p "$base/fixtures/device" "$base/fixtures/immutable" "$base/fixtures/append-only"
sudo -n mknod "$base/fixtures/device/null" c 1 3
echo data >"$base/fixtures/immutable/file"
sudo -n chattr +i "$base/fixtures/immutable/file"
echo data >"$base/fixtures/append-only/file"
sudo -n chattr +a "$base/fixtures/append-only/file"
say DDC_FIXTURE_DEVICE "$base/fixtures/device/null"
say DDC_FIXTURE_IMMUTABLE "$base/fixtures/immutable/file"
say DDC_FIXTURE_APPEND_ONLY "$base/fixtures/append-only/file"
say DDC_MOUNTS "$base"

table="$(for m in btrfs xfs casefold exfat tmpfs tmpfs-root-top zfs zfs/child nfs nfs-squash cifs bind-root/inner; do
    printf '| %s | %s |\n' "$m" "$(stat -f -c %T "$base/$m")"
done; printf '| autofs/trigger | autofs (from mountinfo, never stat'"'"'ed) |\n')"
printf '%s\n' "$table" >&2
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    { echo "### CI mounts"; echo; echo "| mount | filesystem |"; echo "|---|---|"; printf '%s\n' "$table"; } >>"$GITHUB_STEP_SUMMARY"
fi
