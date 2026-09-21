#!/bin/bash
# Runs INSIDE a disposable ubuntu:24.04 container (started by scripts/test-apply-docker.sh).
# Nothing here touches the host.
#
# It builds a local apt repo with a real .deb in two versions, installs 1.0, publishes 2.0 (with a
# changed default config file that the "administrator" has edited), then drives the real aborad and
# the real abora-apply against the real apt-get and checks the outcome with dpkg.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive LC_ALL=C
B=/bin-under-test
step() { echo; echo "== $*"; }

step "tools"
apt-get update -qq >/dev/null && apt-get install -y -qq dpkg-dev curl ca-certificates python3 >/dev/null

step "build a local repo with abora-testpkg 1.0 (and later 2.0)"
mkpkg() { # version conf-content
  d=/tmp/pkg-$1; rm -rf $d; mkdir -p $d/DEBIAN $d/etc $d/usr/share/abora-testpkg
  printf 'Package: abora-testpkg\nVersion: %s\nArchitecture: all\nMaintainer: test <t@example.com>\nDescription: test package\n' "$1" > $d/DEBIAN/control
  echo "$2" > $d/etc/abora-testpkg.conf; echo /etc/abora-testpkg.conf > $d/DEBIAN/conffiles
  echo "$1" > $d/usr/share/abora-testpkg/version
  dpkg-deb --build $d /repo/abora-testpkg_$1_all.deb >/dev/null
}
mkdir -p /repo; mkpkg 1.0 "packaged=1.0"
reindex() { (cd /repo && dpkg-scanpackages . /dev/null > Packages 2>/dev/null); }
reindex
echo 'deb [trusted=yes] file:/repo ./' > /etc/apt/sources.list.d/local.list
apt-get update -qq >/dev/null
apt-get install -y -qq abora-testpkg >/dev/null
echo "installed: $(dpkg-query -W -f='${Version}' abora-testpkg)"
echo "mine=admin-edit" > /etc/abora-testpkg.conf      # the administrator edits the config file

step "publish 2.0 (new default config, so a naive upgrade would ask about the conflict)"
mkpkg 2.0 "packaged=2.0"; reindex; apt-get update -qq >/dev/null
apt-get -s upgrade | grep '^Inst abora-testpkg'

step "configure abora: window covering now, token with manage:updates"
mkdir -p /etc/abora /var/lib/abora /var/lib/abora-apply
SECRET=real-secret-$$; HASH=$(printf %s "$SECRET" | sha256sum | cut -d' ' -f1)
printf '[[tokens]]\nname = "real-test"\npermissions = ["manage:updates", "read:updates"]\nsecret_hash = "sha256:%s"\n' "$HASH" > /etc/abora/tokens.toml; chmod 600 /etc/abora/tokens.toml
DAY=$(date +%u); NAMES=(x monday tuesday wednesday thursday friday saturday sunday)
START=$(date -d '-2 min' +%H:%M); END=$(date -d '+30 min' +%H:%M)
# a window must not cross midnight; if it would, shrink to a valid range
if [ "$START" \> "$END" ]; then START=00:00; END=23:59; fi
sed -e 's#127.0.0.1:7360#127.0.0.1:7396#' /config.toml > /etc/abora/abora.toml
python3 - "${NAMES[$DAY]}" "$START" "$END" <<'PY'
import sys,re
day,start,end=sys.argv[1:]
s=open('/etc/abora/abora.toml').read()
s=re.sub(r'(?m)^\[security\]\n','[security]\ntoken_file = "/etc/abora/tokens.toml"\n',s,count=1)
s+=f'\n[[maintenance.windows]]\ndays = ["{day}"]\nstart = "{start}"\nend = "{end}"\n'
open('/etc/abora/abora.toml','w').write(s)
PY
$B/abora config check /etc/abora/abora.toml | tail -1 >/dev/null && echo "config ok, window ${NAMES[$DAY]} $START-$END"

step "start aborad (real apt-get for the check) and ask it to apply"
$B/aborad --config /etc/abora/abora.toml > /tmp/aborad.log 2>&1 &
sleep 3
API=http://127.0.0.1:7396/api/v1; H="Authorization: Bearer $SECRET"
curl -s -H "$H" $API/updates | python3 -c "import sys,json; d=json.load(sys.stdin); print('found:', [u['component'] for u in d['available']], '| status:', d['status']['state'])"
echo "dry run: $(curl -s -X POST -H "$H" "$API/updates/apply?dry_run=true")"
echo "apply:   $(curl -s -w ' [%{http_code}]' -X POST -H "$H" $API/updates/apply)"
ls -l /var/lib/abora/apply-request.json

step "run the real helper (what the systemd path unit would start)"
$B/abora-apply --config /etc/abora/abora.toml
echo "-- result file:"; cat /var/lib/abora-apply/result.json; echo

step "verify with dpkg"
fail() { echo "FAIL: $*" >&2; exit 1; }
[ "$(dpkg-query -W -f='${Version}' abora-testpkg)" = 2.0 ] || fail "abora-testpkg was not upgraded to 2.0"
[ "$(cat /etc/abora-testpkg.conf)" = "mine=admin-edit" ] || fail "the administrator's config file was overwritten"
[ "$(ls /var/lib/abora | grep -c apply-request || true)" = 0 ] || fail "the request file was left behind"
[ -z "$(dpkg --audit)" ] || fail "dpkg reports a problem"
echo "ok: upgraded to 2.0, admin's config kept (--force-confold), request removed, dpkg audit clean"

step "history through the API (aborad picks the result up within ~30s)"
sleep 34
curl -s -H "$H" $API/updates | python3 -c "
import sys,json
d=json.load(sys.stdin)
assert d['status']['state']=='up_to_date', d['status']
assert d['available']==[], d['available']
assert len(d['history'])==1 and d['history'][0]['succeeded'], d['history']
print('ok: status up_to_date, nothing left available, history:', d['history'][0]['note'])"
grep -q '"message":"audit: real-test requested applying 1 package' /tmp/aborad.log || fail "no audit line naming the token"
echo "ok: audit line names the token"
step "defense in depth: a FORGED request outside the window is refused by the helper itself"
mkpkg 3.0 "packaged=3.0"; reindex; apt-get update -qq >/dev/null
python3 - "${NAMES[$(( DAY % 7 + 1 ))]}" <<'PY'
import sys,re
day=sys.argv[1]
p='/etc/abora/abora.toml'; s=open(p).read()
s=re.sub(r'(?m)^days = \[".*"\]$','days = ["%s"]'%day,s)   # the window is now on a different day
open(p,'w').write(s)
PY
# write the request file directly, bypassing aborad's own checks (what a compromised aborad could do)
printf '{"id":"forged-0001abcd","requested_at":"now","requested_by":"attacker","packages":["abora-testpkg"]}' > /var/lib/abora/apply-request.json
$B/abora-apply --config /etc/abora/abora.toml
grep -q '"succeeded": false' /var/lib/abora-apply/result.json || fail "the helper accepted a forged request outside the window"
grep -q "maintenance window" /var/lib/abora-apply/result.json || fail "the refusal did not say why"
[ "$(dpkg-query -W -f='${Version}' abora-testpkg)" = 2.0 ] || fail "the package changed despite the refusal"
echo "ok: forged request refused (outside the window); abora-testpkg is still 2.0 although 3.0 is available"

step "and a hostile package name in a forged request never reaches apt"
printf '{"id":"forged-0002abcd","requested_at":"now","requested_by":"attacker","packages":["abora-testpkg","--yes"]}' > /var/lib/abora/apply-request.json
$B/abora-apply --config /etc/abora/abora.toml
grep -q '"succeeded": false' /var/lib/abora-apply/result.json || fail "the helper accepted a hostile package name"
[ "$(dpkg-query -W -f='${Version}' abora-testpkg)" = 2.0 ] || fail "the package changed despite the refusal"
echo "ok: hostile package name refused"

echo; echo "ALL GOOD"
