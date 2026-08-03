#!/usr/bin/env bash
#
# Manual validation harness for the reverse-tunnel recipe documented in
# docs/remote-recipes.md ("Reaching networks only your laptop can see").
#
# NOT run by CI. It needs docker and python3, builds an image, and binds host
# ports — deliberately a maintainer-run aid rather than part of any test tier.
# PRD #345 (`remote doctor`) reuses this shape for its e2e coverage, because it
# reproduces every failure mode that command is meant to diagnose.
#
# The shape being tested: the laptop is the network-privileged party. An HTTP
# service bound to the laptop's 127.0.0.1 stands in for an internal git host
# behind a corporate VPN; a docker container (separate network namespace, so it
# genuinely cannot reach that service) stands in for the deck VM.
#
# Two findings from issue #97 are baked into the container build below and are
# the reason this script exists rather than a paragraph of prose:
#   - Alpine's openssh ships AllowTcpForwarding=no, and the resulting error is
#     byte-identical to a port collision.
#   - sshd_config takes the FIRST value per keyword, so appending an override
#     to the end of the file silently does nothing.
#
# Usage: scripts/reverse-tunnel-validation.sh
# Exits 0 when every check passes.
set -uo pipefail

WORK="$(mktemp -d)"
KEY="$WORK/id_test"
CFG="$WORK/ssh_config"
SENTINEL="tunnel-sentinel-97-ok"
HTTP_PORT=18080
SOCKS_PORT=11080
CTR=dad-tunnel-test
IMG=dad-tunnel-test

cleanup() {
  pkill -f "ssh -F $WORK" 2>/dev/null
  [ -n "${HTTP_PID:-}" ] && kill "$HTTP_PID" 2>/dev/null
  docker rm -f "$CTR" >/dev/null 2>&1
  rm -rf "$WORK"
}
trap cleanup EXIT

pass() { echo "  PASS  $*"; }
fail() { echo "  FAIL  $*"; FAILED=1; }
FAILED=0

for dep in docker python3 ssh ssh-keygen curl; do
  command -v "$dep" >/dev/null || { echo "missing dependency: $dep"; exit 1; }
done

echo "== setup =="
ssh-keygen -t ed25519 -N '' -f "$KEY" -q
echo "keypair generated"

# Laptop-only HTTP service: binds 127.0.0.1, so nothing outside the laptop's
# network namespace can reach it. This is the "git.company.com" stand-in.
WEBROOT="$WORK/webroot"; mkdir -p "$WEBROOT"; echo "$SENTINEL" > "$WEBROOT/index.html"
python3 -m http.server "$HTTP_PORT" --bind 127.0.0.1 --directory "$WEBROOT" >/dev/null 2>&1 &
HTTP_PID=$!
sleep 1
curl -sS --max-time 5 "http://127.0.0.1:$HTTP_PORT/" | grep -q "$SENTINEL" \
  && echo "laptop-only service up on 127.0.0.1:$HTTP_PORT" \
  || { echo "could not start laptop service"; exit 1; }

# Container stands in for the deck VM.
cat > "$WORK/Dockerfile" <<'EOF'
FROM alpine:3.20
RUN apk add --no-cache openssh curl && ssh-keygen -A && adduser -D deck
# adduser -D leaves '!' in /etc/shadow, which sshd treats as a locked account
# and refuses even for pubkey auth. '*' = no password login, but not locked.
RUN sed -i 's/^deck:!:/deck:*:/' /etc/shadow
# Alpine's openssh package ships AllowTcpForwarding=no, which makes every
# remote forward fail with an opaque "remote port forwarding failed". Ubuntu
# defaults to yes, but CIS/hardening baselines commonly turn it off.
# NOTE: sshd_config takes the FIRST obtained value per keyword, so appending
# at the end does nothing when the key is already set above. Rewrite in place.
RUN sed -i 's/^#\?[[:space:]]*AllowTcpForwarding.*/AllowTcpForwarding yes/' /etc/ssh/sshd_config
RUN mkdir -p /home/deck/.ssh && chmod 700 /home/deck/.ssh
COPY id_test.pub /home/deck/.ssh/authorized_keys
RUN chown -R deck:deck /home/deck/.ssh && chmod 600 /home/deck/.ssh/authorized_keys
CMD ["/usr/sbin/sshd", "-D", "-e"]
EOF
docker build -q -t "$IMG" "$WORK" >/dev/null 2>&1 || { echo "docker build failed"; exit 1; }
docker rm -f "$CTR" >/dev/null 2>&1
docker run -d --name "$CTR" -p 127.0.0.1:2222:22 "$IMG" >/dev/null || exit 1
sleep 3
echo "container VM up on 127.0.0.1:2222"

cat > "$CFG" <<EOF
Host deck-test
    HostName 127.0.0.1
    Port 2222
    User deck
    IdentityFile $KEY
    IdentitiesOnly yes
    StrictHostKeyChecking no
    UserKnownHostsFile /dev/null
    RemoteForward $SOCKS_PORT
    ExitOnForwardFailure yes
EOF

# Mirror src/connect.rs build_connect_command. -F only points ssh at a test
# config instead of the user's real ~/.ssh/config; the deck passes no -F, which
# is precisely why a real ~/.ssh/config Host block applies to `connect`.
deck_ssh() {
  ssh -F "$CFG" -t \
      -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=3 \
      -p 2222 -i "$KEY" -- deck-test "$@" 2>/dev/null
}

echo
echo "== T0: ssh -G exposes the resolved forward (basis for PRD #345) =="
ssh -F "$CFG" -G deck-test | grep -qi "^remoteforward $SOCKS_PORT \[socks\]:0" \
  && pass "ssh -G reports 'remoteforward $SOCKS_PORT [socks]:0'" \
  || fail "ssh -G did not report the reverse-SOCKS forward"

echo
echo "== T1: the VM genuinely cannot reach the laptop-only service =="
OUT=$(docker exec "$CTR" curl -sS --max-time 5 "http://127.0.0.1:$HTTP_PORT/" 2>&1)
echo "$OUT" | grep -q "$SENTINEL" \
  && fail "VM reached the service directly - asymmetry not established" \
  || pass "VM cannot reach it directly (as expected)"

echo
echo "== T2: through the reverse-SOCKS tunnel, the VM CAN reach it =="
OUT=$(deck_ssh "sleep 1; curl -sS --max-time 8 --socks5-hostname 127.0.0.1:$SOCKS_PORT http://127.0.0.1:$HTTP_PORT/")
echo "$OUT" | grep -q "$SENTINEL" \
  && pass "VM fetched the sentinel through the laptop (socks5h)" \
  || fail "tunnel did not carry the request; got: $(echo "$OUT" | head -3)"

echo
echo "== T3: DynamicForward is the WRONG direction (listener lands on laptop) =="
# Separate config: DynamicForward INSTEAD of RemoteForward, so there is no
# ambiguity about which directive produced which listener.
sed "s/^    RemoteForward $SOCKS_PORT/    DynamicForward 9099/" "$CFG" > "$CFG.dyn"
ssh -F "$CFG.dyn" -f -N deck-test 2>/dev/null
sleep 2
LAPTOP_HAS=$(ss -ltn 2>/dev/null | grep -c ':9099')
VM_HAS=$(docker exec "$CTR" sh -c "netstat -ltn 2>/dev/null | grep -c ':9099'")
[ "$LAPTOP_HAS" -gt 0 ] && [ "$VM_HAS" -eq 0 ] \
  && pass "DynamicForward listener is on the LAPTOP, not the VM" \
  || fail "unexpected: laptop=$LAPTOP_HAS vm=$VM_HAS"
pkill -f "ssh -F $WORK" 2>/dev/null; sleep 1

echo
echo "== T4: second session collides on the same forward port =="
ssh -F "$CFG" -f -N deck-test 2>/dev/null
sleep 2
# Guard: the first session must have ACTUALLY bound. If forwarding were
# refused wholesale (e.g. AllowTcpForwarding no) both sessions would fail with
# the same "remote port forwarding failed" text and the collision assertion
# below would pass for entirely the wrong reason. This bit us during #97.
docker exec "$CTR" sh -c "netstat -ltn 2>/dev/null | grep -q ':$SOCKS_PORT'" \
  || { fail "first session never bound the forward - collision test is meaningless"; SKIP_T4=1; }
ERR=$(ssh -F "$CFG" -o ConnectTimeout=10 deck-test true 2>&1); RC=$?
# Require the FORWARD-specific diagnostic. A bare non-zero rc is ambiguous:
# an auth failure also exits 255 and would give a false pass. This bit us too.
FWD_ERR=$(echo "$ERR" | grep -i 'forward\|listen\|address already in use' | head -2 | tr '\n' ' ')
if [ -n "${SKIP_T4:-}" ]; then
  :
elif [ $RC -ne 0 ] && [ -n "$FWD_ERR" ]; then
  pass "second session refused (rc=$RC) with ExitOnForwardFailure=yes"
  echo "        ssh said: $FWD_ERR"
else
  fail "collision not reproduced (rc=$RC); ssh said: $(echo "$ERR" | head -2 | tr '\n' ' ')"
fi

echo
[ "$FAILED" -eq 0 ] && echo "ALL CHECKS PASSED" || echo "SOME CHECKS FAILED"
exit $FAILED
