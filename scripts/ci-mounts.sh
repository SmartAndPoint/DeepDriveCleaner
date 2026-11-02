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
# `sudo -n`, which fails instead of asking. It builds the CLI of this checkout once, for
# the scan a remount must not make stale. CI only: it edits /etc/exports and Samba's
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

# A btrfs scan root (story "btrfs and ZFS on Linux: subvolumes, datasets, snapshots and
# stable identities", DDC-010): a plain subvolume with a project and a nested one; a
# node_modules that is a subvolume and one that holds one; read-only, writable and
# received snapshots; and a second btrfs mounted inside. All made by the user but the
# send and receive and the inner mount, which need root.
loop btrfs-tree 300 mkfs.btrfs -q
t="$base/btrfs-tree"
btrfs subvolume create "$t/work" >/dev/null
mkdir -p "$t/work/app/node_modules/dep"
head -c 65536 /dev/zero >"$t/work/app/node_modules/dep/index.js"
btrfs subvolume create "$t/work/nested" >/dev/null
head -c 65536 /dev/zero >"$t/work/nested/file"
mkdir -p "$t/whole" "$t/holds/node_modules"
btrfs subvolume create "$t/whole/node_modules" >/dev/null
echo data >"$t/whole/node_modules/index.js"
btrfs subvolume create "$t/holds/node_modules/inner" >/dev/null
echo data >"$t/holds/node_modules/inner/index.js"
btrfs subvolume snapshot -r "$t/work" "$t/snap-ro" >/dev/null
btrfs subvolume snapshot "$t/work" "$t/snap-rw" >/dev/null
mkdir -p "$t/recv"
sudo -n btrfs send -q "$t/snap-ro" | sudo -n btrfs receive -q "$t/recv"
truncate -s 300M "$img/btrfs-inner.img"
mkfs.btrfs -q "$img/btrfs-inner.img" >/dev/null
mkdir -p "$t/data"
sudo -n mount -o loop "$img/btrfs-inner.img" "$t/data"
mine "$t/data"
say DDC_MOUNT_BTRFS_TREE "$t"

# A btrfs mounted again under a saved scan (the same story): a subvolume `home` with two
# projects, one whose node_modules is kept, scanned by this checkout's ddc into
# `state/data`; then the filesystem is unmounted and mounted again until `home` has
# another anonymous device, each try holding one more freed device with a small tmpfs.
# The test reads the scan and the kept folder's identity it recorded after the remount.
loop btrfs-remount 300 mkfs.btrfs -q
r="$base/btrfs-remount"
btrfs subvolume create "$r/home" >/dev/null
mkdir -p "$r/state"
for p in p q; do
    mkdir -p "$r/home/$p/node_modules/dep"
    echo '{}' >"$r/home/$p/package.json"
    echo '{}' >"$r/home/$p/package-lock.json"
    touch -d '60 days ago' "$r/home/$p/package.json" "$r/home/$p/package-lock.json"
    head -c 65536 /dev/zero >"$r/home/$p/node_modules/dep/index.js"
done
: >"$r/home/q/node_modules/.ddc-keep"
repo="$(cd "$(dirname "$0")/.." && pwd -P)"
cargo build --locked -q --manifest-path "$repo/Cargo.toml" -p ddc-cli --features ddc-core/op-stats >&2
env -i PATH= NO_COLOR=1 HOME="$r/home" DDC_DATA_DIR="$r/state/data" \
    "${CARGO_TARGET_DIR:-$repo/target}/debug/deepdrivecleaner" scan >/dev/null
[ -f "$r/state/data/last-scan.json" ] || fail "the scan before the remount saved no last-scan.json"
before="$(stat -c %d "$r/home")"
echo "$before" >"$r/state/before-dev"
moved=
for i in 1 2 3 4 5 6 7 8; do
    sudo -n umount "$r"
    mkdir -p "$base/remount-hold/$i"
    sudo -n mount -t tmpfs -o size=1m tmpfs "$base/remount-hold/$i"
    sudo -n mount -o loop "$img/btrfs-remount.img" "$r"
    if [ "$(stat -c %d "$r/home")" != "$before" ]; then
        moved=1
        break
    fi
done
[ -n "$moved" ] || fail "btrfs-remount: home kept the anonymous device $before through 8 remounts"
say DDC_MOUNT_BTRFS_REMOUNT "$r/home"

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

# A ZFS scan root (story "btrfs and ZFS on Linux: subvolumes, datasets, snapshots and
# stable identities", DDC-010): a child dataset with a project and a grandchild; a
# node_modules that is a dataset and one that holds one, each below a dataset that is
# never mounted (canmount=off); a dataset with snapdir=visible and a snapshot; a bind
# mount of a dataset's mount point; and a child mounted where it does not inherit
# (mountpoint= of its own). Then a home for the delete test, with a child dataset, and
# one for the guard test, whose node_modules is a dataset. Compression off, and random
# bytes: OpenZFS 2.2 compresses by default and stores a block of zeros as a hole, and a
# tiny file in its block pointer, so the byte counts the tests check would be zero.
z="$base/zfs/tree"
sudo -n zfs create -o compression=off ddcmounts/tree
sudo -n zfs create ddcmounts/tree/work
sudo -n zfs create ddcmounts/tree/work/nested
sudo -n zfs create -o canmount=off ddcmounts/tree/whole
sudo -n zfs create ddcmounts/tree/whole/node_modules
sudo -n zfs create -o canmount=off ddcmounts/tree/holds
sudo -n zfs create -o canmount=off ddcmounts/tree/holds/node_modules
sudo -n zfs create ddcmounts/tree/holds/node_modules/inner
sudo -n zfs create -o snapdir=visible ddcmounts/tree/snapdir
sudo -n zfs create -o mountpoint="$z/moved" ddcmounts/tree/away
for d in "$z" "$z/work" "$z/work/nested" "$z/whole/node_modules" "$z/holds/node_modules/inner" "$z/snapdir" "$z/moved"; do
    mine "$d"
done
mkdir -p "$z/work/app/node_modules/dep" "$z/bound"
head -c 65536 /dev/urandom >"$z/work/app/node_modules/dep/index.js"
head -c 65536 /dev/urandom >"$z/work/nested/file"
echo data >"$z/whole/node_modules/index.js"
echo data >"$z/holds/node_modules/inner/index.js"
echo data >"$z/snapdir/file"
echo data >"$z/moved/file"
sudo -n zfs snapshot ddcmounts/tree/snapdir@ddc
sudo -n mount --bind "$z/work" "$z/bound"
sudo -n zfs create ddcmounts/del
sudo -n zfs create ddcmounts/del/work
mine "$base/zfs/del"
mine "$base/zfs/del/work"
sudo -n zfs create ddcmounts/guard
sudo -n zfs create -o canmount=off ddcmounts/guard/p
sudo -n zfs create ddcmounts/guard/p/node_modules
mine "$base/zfs/guard"
mine "$base/zfs/guard/p"
mine "$base/zfs/guard/p/node_modules"
say DDC_MOUNT_ZFS_TREE "$z"
say DDC_MOUNT_ZFS_DELETE "$base/zfs/del"
say DDC_MOUNT_ZFS_GUARD "$base/zfs/guard"

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

# What a delete frees where the filesystem compresses (story "btrfs and ZFS on Linux:
# subvolumes, datasets, snapshots and stable identities", DDC-010). 16 MiB is written
# twice, as text that compresses well and as random bytes that do not, in a btrfs folder
# with zstd and in a ZFS dataset with lz4. Each file is measured by this checkout's
# `ddc du`, then removed, and the space in use is read before and after, each time once
# the filesystem has committed and two readings in a row agree. The table goes to stderr
# and to the step summary; the checks below it hold the direction the runs showed. The
# first run: btrfs counted 16 MiB of text and freed 512 KiB, ZFS counted 136 KiB and
# freed 135 KiB, and random bytes freed what was counted on both.
loop btrfs-zstd 300 mkfs.btrfs -q
mkdir "$base/btrfs-zstd/c"
sudo -n btrfs property set "$base/btrfs-zstd/c" compression zstd
sudo -n zfs create -o compression=lz4 ddcmounts/comp
mine "$base/zfs/comp"
ddc="${CARGO_TARGET_DIR:-$repo/target}/debug/deepdrivecleaner"
btrfs_used() {
    local blocks free size
    sync -f "$1"
    sudo -n btrfs filesystem sync "$1" >/dev/null
    read -r blocks free size < <(stat -f -c '%b %f %S' "$1")
    echo $(((blocks - free) * size))
}
zfs_used() {
    sudo -n zpool sync ddcmounts
    zfs get -Hp -o value used ddcmounts/comp
}
settled() {
    local prev now i
    prev="$("$@")"
    for i in $(seq 30); do
        sleep 1
        now="$("$@")"
        if [ "$now" = "$prev" ]; then
            echo "$now"
            return
        fi
        prev="$now"
    done
    fail "$*: the space in use did not settle in 30 s (last $prev, then $now)"
}
ctable=
# $1 filesystem, $2 its folder, $3 text or random, $4… prints the bytes in use there.
measure() {
    local fs="$1" kind="$3" dir="$2/$3" before after bytes freed out
    shift 3
    mkdir "$dir"
    if [ "$kind" = text ]; then
        head -c 16M < <(yes 'ddc measures what a delete frees where the filesystem compresses') >"$dir/f"
    else
        head -c 16M /dev/urandom >"$dir/f"
    fi
    before="$(settled "$@")"
    out="$(env -i PATH= NO_COLOR=1 HOME="$dir" "$ddc" du "$dir" 2>"$img/du.err")" ||
        fail "ddc du $dir failed: $(cat "$img/du.err")"
    grep -q ' 0 errors, 0 skipped,' "$img/du.err" || fail "ddc du $dir: $(cat "$img/du.err")"
    bytes=$(($(cut -f1 <<<"$out") * 1024))
    rm -r "$dir"
    after="$(settled "$@")"
    freed=$((before - after))
    ctable+="$(printf '| %s | %s | %d | %d | %s |' "$fs" "$kind" "$bytes" "$freed" \
        "$(awk -v b="$bytes" -v f="$freed" 'BEGIN { printf "%.2f", (f > 0 ? b / f : 0) }')")"$'\n'
    eval "m_${fs}_${kind}_bytes=$bytes m_${fs}_${kind}_freed=$freed"
}
measure btrfs "$base/btrfs-zstd/c" text btrfs_used "$base/btrfs-zstd"
measure btrfs "$base/btrfs-zstd/c" random btrfs_used "$base/btrfs-zstd"
measure zfs "$base/zfs/comp" text zfs_used
measure zfs "$base/zfs/comp" random zfs_used
printf '| filesystem | data | ddc bytes | freed by the delete | bytes / freed |\n%s' "$ctable" >&2
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    { echo "### What a delete frees where the filesystem compresses"; echo
      echo "| filesystem | data | ddc bytes | freed by the delete | bytes / freed |"; echo "|---|---|---|---|---|"
      printf '%s' "$ctable"; } >>"$GITHUB_STEP_SUMMARY"
fi
kib=1024
mib=$((1 << 20))
# Counted $1 against freed $2: at most 2% under what the delete freed, at most 10% over.
near() { [ $(($1 * 100)) -ge $(($2 * 98)) ] && [ $(($1 * 10)) -le $(($2 * 11)) ]; }
[ "$m_btrfs_text_bytes" -ge $((16 * mib)) ] || fail "compressed btrfs: ddc counted $m_btrfs_text_bytes bytes of 16 MiB of text, not its uncompressed size"
[ "$m_btrfs_text_freed" -lt $((4 * mib)) ] || fail "compressed btrfs: the delete freed $m_btrfs_text_freed bytes, as if the text had not been compressed"
[ "$m_btrfs_text_freed" -ge $((128 * kib)) ] || fail "compressed btrfs: the delete freed only $m_btrfs_text_freed bytes, less than the compressed text"
[ "$m_zfs_text_bytes" -lt $((4 * mib)) ] || fail "ZFS with lz4: ddc counted $m_zfs_text_bytes bytes of 16 MiB of text, not its compressed size"
d=$((m_zfs_text_bytes - m_zfs_text_freed))
[ "${d#-}" -le $((16 * kib)) ] || fail "ZFS with lz4: ddc counted $m_zfs_text_bytes bytes of text and the delete freed $m_zfs_text_freed, more than 16 KiB apart"
for fs in btrfs zfs; do
    eval "b=\$m_${fs}_random_bytes f=\$m_${fs}_random_freed"
    near "$b" "$f" || fail "$fs: random bytes were counted as $b and freed $f, more than 2% under or 10% over"
done

say DDC_MOUNTS "$base"

table="$(for m in btrfs btrfs-tree btrfs-zstd btrfs-tree/data btrfs-remount/home xfs casefold exfat tmpfs tmpfs-root-top zfs zfs/child zfs/tree zfs/tree/work zfs/tree/moved zfs/del/work zfs/guard/p/node_modules zfs/comp nfs nfs-squash cifs bind-root/inner; do
    printf '| %s | %s |\n' "$m" "$(stat -f -c %T "$base/$m")"
done; printf '| autofs/trigger | autofs (from mountinfo, never stat'"'"'ed) |\n')"
printf '%s\n' "$table" >&2
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    { echo "### CI mounts"; echo; echo "| mount | filesystem |"; echo "|---|---|"; printf '%s\n' "$table"; } >>"$GITHUB_STEP_SUMMARY"
fi
