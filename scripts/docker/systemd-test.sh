#!/bin/bash
# Runs INSIDE a disposable ubuntu:24.04 container that is running systemd as PID 1
# (started by scripts/test-systemd-docker.sh). Nothing here touches the host.
#
# This is the "real deployment" test: the real installer as root (useradd, unit files), aborad
# under its real hardened unit as the unprivileged aborad user, the systemd path unit starting the
# root helper, a real .deb upgraded by real apt, then the real uninstaller.
set -euo pipefail
export LC_ALL=C DEBIAN_FRONTEND=noninteractive
BIN=/bin-under-test
fail() { echo "FAIL: $*" >&2; journalctl -u aborad -u abora-apply --no-pager -n 30 >&2 || true; exit 1; }
step() { echo; echo "== $*"; }
wait_for() { # description, timeout-seconds, command...
  local what=$1 secs=$2; shift 2
  for _ in $(seq 1 "$secs"); do "$@" >/dev/null 2>&1 && return 0; sleep 1; done
  fail "timed out waiting for: $what"
}

step "a real .deb, version 1.0 installed, 2.0 published in a local repo"
mkpkg() {
  d=/tmp/pkg-$1; rm -rf $d; mkdir -p $d/DEBIAN
  printf 'Package: abora-testpkg\nVersion: %s\nArchitecture: all\nMaintainer: t <t@example.com>\nDescription: test package\n' "$1" > $d/DEBIAN/control
  dpkg-deb --build $d /repo-debs/abora-testpkg_$1_all.deb >/dev/null
}
mkdir -p /repo-debs; mkpkg 1.0
reindex() { (cd /repo-debs && dpkg-scanpackages . /dev/null > Packages 2>/dev/null); }
reindex
echo 'deb [trusted=yes] file:/repo-debs ./' > /etc/apt/sources.list.d/local.list
apt-get update -qq >/dev/null && apt-get install -y -qq abora-testpkg >/dev/null
mkpkg 2.0; reindex; apt-get update -qq >/dev/null
echo "installed $(dpkg-query -W -f='${Version}' abora-testpkg), 2.0 is available"

step "the real installer, as root"
cd /repo && ./installer/install.sh --bin-dir $BIN --no-start >/dev/null
getent passwd aborad >/dev/null || fail "the aborad user was not created"
[ "$(stat -c '%U:%G %a' /etc/abora)" = "root:aborad 750" ] || fail "/etc/abora has wrong ownership/mode: $(stat -c '%U:%G %a' /etc/abora)"
systemctl is-enabled aborad abora-apply.path >/dev/null || fail "units are not enabled"
echo "ok: user created, /etc/abora is root:aborad 0750, units enabled"

step "configure a token (readable by the aborad group) and a maintenance window covering now"
SECRET=sd-secret-$$; HASH=$(printf %s "$SECRET" | sha256sum | cut -d' ' -f1)
printf '[[tokens]]\nname = "systemd-test"\npermissions = ["manage:updates", "read:updates"]\nsecret_hash = "sha256:%s"\n' "$HASH" > /etc/abora/tokens.toml
chown root:aborad /etc/abora/tokens.toml; chmod 640 /etc/abora/tokens.toml
DAY=$(date +%u); NAMES=(x monday tuesday wednesday thursday friday saturday sunday)
START=$(date -d '-2 min' +%H:%M); END=$(date -d '+30 min' +%H:%M)
if [ "$START" \> "$END" ]; then START=00:00; END=23:59; fi
python3 - "${NAMES[$DAY]}" "$START" "$END" <<'PY'
import sys,re
day,start,end=sys.argv[1:]
p='/etc/abora/abora.toml'; s=open(p).read()
s=re.sub(r'(?m)^\[security\]\n','[security]\ntoken_file = "/etc/abora/tokens.toml"\n',s,count=1)
s+=f'\n[[maintenance.windows]]\ndays = ["{day}"]\nstart = "{start}"\nend = "{end}"\n'
open(p,'w').write(s)
PY
$BIN/abora config check /etc/abora/abora.toml >/dev/null || fail "config did not validate"

step "start the service and the path unit"
systemctl start aborad abora-apply.path
wait_for "aborad to answer" 15 curl -sf http://127.0.0.1:7360/api/v1/updates -H "Authorization: Bearer $SECRET"
export ABORA_DAEMON_TOKEN=$SECRET
wait_for "the first update check" 20 bash -c '/usr/bin/abora updates | grep -q "1 update(s) available"'
/usr/bin/abora updates
[ "$(ps -o user= -C aborad)" = aborad ] || fail "aborad is not running as the aborad user"
echo "ok: aborad runs as the unprivileged aborad user under the real unit, and its apt check works inside the sandbox"

step "abora updates apply --yes: aborad writes the request, the systemd path unit starts the root helper"
/usr/bin/abora updates apply --yes
wait_for "the helper to finish" 40 bash -c 'test "$(systemctl show abora-apply.service -p ActiveState --value)" = inactive && test -e /var/lib/abora-apply/result.json'
[ "$(dpkg-query -W -f='${Version}' abora-testpkg)" = 2.0 ] || fail "abora-testpkg was not upgraded"
[ "$(systemctl show abora-apply.service -p Result --value)" = success ] || fail "the helper unit did not finish with success"
[ ! -e /var/lib/abora/apply-request.json ] || fail "the request file was left behind"
[ "$(stat -c '%U %a' /var/lib/abora-apply/result.json)" = "root 644" ] || fail "result file should be root-owned 0644"
[ "$(stat -c '%U %a' /var/lib/abora-apply)" = "root 755" ] || fail "result directory should be root-owned 0755"
journalctl -u abora-apply --no-pager -o cat | grep -q "succeeded: 1 package" || fail "no success line in the helper's journal"
echo "ok: upgraded to 2.0 by the path-unit-started helper; result is root-owned; request removed"

step "aborad picks up the result and records it (within ~30s)"
wait_for "the history entry" 45 bash -c '/usr/bin/abora updates | grep -q "apply apply-"'
/usr/bin/abora updates
journalctl -u aborad --no-pager -o cat | grep -q "audit: systemd-test requested applying 1 package" || fail "no audit line naming the token"
echo "ok: history recorded, audit line names the token"

step "the sandbox: aborad's user cannot write where it must not"
for path in /var/lib/abora-apply/probe /etc/abora/probe /usr/sbin/probe; do
  if runuser -u aborad -- touch $path 2>/dev/null; then fail "aborad's user could write $path"; fi
done
echo "ok: the aborad user cannot write /var/lib/abora-apply, /etc/abora or /usr/sbin"
SCORE=$(systemd-analyze security aborad.service --no-pager 2>/dev/null | tail -1 || true)
echo "systemd-analyze: $SCORE"

step "re-running the installer keeps the operator's edits"
echo "# operator edit" >> /etc/abora/abora.toml
/repo/installer/install.sh --bin-dir $BIN --no-start >/dev/null
grep -q "# operator edit" /etc/abora/abora.toml || fail "re-install overwrote the operator's config"
echo "ok"

step "the real uninstaller"
/repo/installer/install.sh --uninstall >/dev/null
[ ! -e /usr/sbin/aborad ] && [ ! -e /usr/sbin/abora-apply ] && [ ! -e /etc/systemd/system/aborad.service ] || fail "uninstall left files"
[ -e /etc/abora/abora.toml ] && [ -e /var/lib/abora ] || fail "uninstall removed config or state"
/repo/installer/install.sh --bin-dir $BIN --no-start >/dev/null
/repo/installer/install.sh --uninstall --purge >/dev/null
[ ! -e /etc/abora ] && [ ! -e /var/lib/abora ] && ! getent passwd aborad >/dev/null || fail "purge left config, state or the user"
echo "ok: uninstall keeps data, --purge removes it and the user"

echo; echo "ALL GOOD"
