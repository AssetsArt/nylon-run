#!/bin/bash
set -euo pipefail

PASS=0
FAIL=0
TESTS=()

pass() { echo "  PASS: $1"; PASS=$((PASS + 1)); TESTS+=("PASS: $1"); }
fail() { echo "  FAIL: $1 — $2"; FAIL=$((FAIL + 1)); TESTS+=("FAIL: $1 — $2"); }

wait_for() {
    local url=$1 timeout=${2:-10}
    for i in $(seq 1 $timeout); do
        if curl -sf "$url" >/dev/null 2>&1; then return 0; fi
        sleep 1
    done
    return 1
}

wait_for_json() {
    local url=$1 timeout=${2:-10}
    for i in $(seq 1 $timeout); do
        if curl -sf "$url" 2>/dev/null | jq -e '.status' >/dev/null 2>&1; then return 0; fi
        sleep 1
    done
    return 1
}

wait_for_process() {
    local name=$1 timeout=${2:-8}
    for i in $(seq 1 $timeout); do
        if nyrun ls 2>/dev/null | grep -q "$name"; then return 0; fi
        sleep 1
    done
    return 1
}

echo "========================================="
echo " nyrun integration tests"
echo "========================================="

# ==========================================================
echo ""
echo "[1] process only (no proxy)"
nyrun run python3 --name proc-only --args "/tests/fixtures/http-server.py" 2>/dev/null &
sleep 3
wait_for_process "proc-only" 10 && pass "process started" || fail "process started" "not in ls"
wait_for http://127.0.0.1:8000 10 && pass "responds on :8000" || fail "responds on :8000" "no response"

# ==========================================================
echo ""
echo "[2] ls / logs"
nyrun ls 2>/dev/null | grep -q "proc-only" && pass "ls shows process" || fail "ls" "not found"
LOGS=$(nyrun logs proc-only --lines 5 2>/dev/null || true)
[ -n "$LOGS" ] && pass "logs returns output" || fail "logs" "empty"

# ==========================================================
echo ""
echo "[3] restart"
nyrun restart proc-only 2>/dev/null || true
sleep 2
wait_for_process "proc-only" && pass "restarted" || fail "restarted" "not found"

# ==========================================================
echo ""
echo "[4] del"
nyrun del proc-only 2>/dev/null || true
sleep 1
nyrun ls 2>/dev/null | grep -q "proc-only" && fail "deleted" "still in ls" || pass "deleted"

# ==========================================================
echo ""
echo "[5] proxy (auto-remap same port)"
nyrun run python3 --name proxied --args "/tests/fixtures/http-server.py" --p 7070:7070 2>/dev/null &
sleep 4
wait_for_json http://127.0.0.1:7070 15 && pass "proxy auto-remap" || fail "proxy auto-remap" "no json on :7070"

# ==========================================================
echo ""
echo "[6] proxy (second auto-remap)"
nyrun run python3 --name proxied2 --args "/tests/fixtures/http-server.py" --p 7171:7171 2>/dev/null &
sleep 5
wait_for_json http://127.0.0.1:7171 20 && pass "proxy auto-remap 2" || fail "proxy auto-remap 2" "no json on :7171"

# ==========================================================
echo ""
echo "[7] set"
nyrun set default-registry ghcr.io 2>/dev/null | grep -q "ghcr.io" && pass "set default-registry" || fail "set default-registry" "failed"
nyrun set default-registry docker.io 2>/dev/null || true
nyrun set cache-ttl 120 2>/dev/null | grep -q "120" && pass "set cache-ttl" || fail "set cache-ttl" "failed"

# ==========================================================
echo ""
echo "[8] metrics enable/disable"
nyrun metrics enable --port 9100 2>/dev/null | grep -q "9100" && pass "metrics enable" || fail "metrics enable" "failed"
sleep 1
wait_for http://127.0.0.1:9100 5 && pass "metrics endpoint responds" || fail "metrics endpoint responds" "no response"
nyrun metrics disable 2>/dev/null | grep -q "stopped" && pass "metrics disable" || fail "metrics disable" "failed"

# ==========================================================
echo ""
echo "[9] ecosystem.yaml with ConfigMap"
nyrun del proxied 2>/dev/null || true
nyrun del proxied2 2>/dev/null || true
sleep 1
nyrun start /tests/fixtures/ecosystem.yaml 2>/dev/null &
sleep 8

wait_for_process "web" && wait_for_process "worker" && wait_for_process "with-config" \
    && pass "ecosystem all started" || fail "ecosystem all started" "missing processes"

wait_for_json http://127.0.0.1:7001 15 && pass "ecosystem web proxy" || fail "ecosystem web proxy" "no json on :7001"
wait_for http://127.0.0.1:8002 10 && pass "ecosystem worker direct" || fail "ecosystem worker direct" "no response on :8002"
wait_for_json http://127.0.0.1:7003 15 && pass "ecosystem with-config proxy" || fail "ecosystem with-config proxy" "no json on :7003"

# ==========================================================
echo ""
echo "[10] ConfigMap files"
[ -f /var/run/nyrun/configmaps/test-config/app.conf ] \
    && grep -q "debug=true" /var/run/nyrun/configmaps/test-config/app.conf \
    && pass "configmap app.conf" || fail "configmap app.conf" "missing or wrong"
[ -f /var/run/nyrun/configmaps/test-config/settings.json ] \
    && pass "configmap settings.json" || fail "configmap settings.json" "missing"

# ==========================================================
echo ""
echo "[11] export"
EXPORT=$(nyrun export 2>/dev/null || true)
echo "$EXPORT" | grep -q "kind: Process" && pass "export YAML format" || fail "export YAML format" "no kind: Process"
echo "$EXPORT" | grep -q "web" && pass "export has web" || fail "export has web" "missing"

# ==========================================================
echo ""
echo "[12] save"
nyrun save 2>/dev/null | grep -qi "saved\|ok" && pass "save" || fail "save" "failed"

# ==========================================================
echo ""
echo "[13] backup / restore"
nyrun backup -o /tmp/test-backup 2>/dev/null || true
if [ -f /tmp/test-backup.zip ]; then
    pass "backup created"
    nyrun restore /tmp/test-backup.zip 2>/dev/null | grep -qi "restore\|ok" && pass "restore" || fail "restore" "failed"
else
    fail "backup" "file not found"
fi

# ==========================================================
echo ""
echo "[14] cleanup all"
for name in web worker with-config; do nyrun del "$name" 2>/dev/null || true; done
sleep 1
REMAINING=$(nyrun ls 2>/dev/null | grep -c "Running" || true)
[ "$REMAINING" -eq 0 ] && pass "all cleaned up" || fail "all cleaned up" "$REMAINING running"

# ==========================================================
echo ""
echo "[15] kill"
nyrun kill 2>/dev/null || true
sleep 2
[ ! -f /var/run/nyrun/nyrun.pid ] || ! kill -0 "$(cat /var/run/nyrun/nyrun.pid)" 2>/dev/null \
    && pass "daemon stopped" || fail "daemon stopped" "still running"

# ==========================================================
echo ""
echo "========================================="
echo " Results: $PASS passed, $FAIL failed"
echo "========================================="
echo ""
for t in "${TESTS[@]}"; do echo "  $t"; done
echo ""
[ "$FAIL" -gt 0 ] && exit 1 || exit 0
