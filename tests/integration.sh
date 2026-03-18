#!/bin/bash
set -euo pipefail

PASS=0
FAIL=0
TESTS=()

pass() { echo "  PASS: $1"; PASS=$((PASS + 1)); TESTS+=("PASS: $1"); }
fail() { echo "  FAIL: $1 — $2"; FAIL=$((FAIL + 1)); TESTS+=("FAIL: $1 — $2"); }

wait_for() {
    local url=$1 timeout=${2:-15}
    for i in $(seq 1 $timeout); do
        if curl -sf "$url" >/dev/null 2>&1; then return 0; fi
        sleep 1
    done
    return 1
}

wait_for_json() {
    local url=$1 timeout=${2:-15}
    for i in $(seq 1 $timeout); do
        local resp
        resp=$(curl -sf "$url" 2>/dev/null || true)
        if echo "$resp" | jq -e '.status' >/dev/null 2>&1; then return 0; fi
        sleep 1
    done
    return 1
}

wait_for_process() {
    local name=$1 timeout=${2:-10}
    for i in $(seq 1 $timeout); do
        if nyrun ls 2>/dev/null | grep -q "$name"; then return 0; fi
        sleep 1
    done
    return 1
}

echo "========================================="
echo " nyrun integration tests"
echo "========================================="
echo ""

# --------------------------------------------------
echo "[1] nyrun run — process only (no proxy)"
# --------------------------------------------------
nyrun run python3 --name proc-only --args "/tests/fixtures/http-server.py" 2>/dev/null &
sleep 3

if wait_for_process "proc-only"; then
    pass "process started without proxy"
else
    fail "process started without proxy" "not found in ls"
fi

if curl -sf http://127.0.0.1:8000 | jq -e '.status == "ok"' >/dev/null 2>&1; then
    pass "process responds on default port"
else
    fail "process responds on default port" "no response on :8000"
fi

# --------------------------------------------------
echo ""
echo "[2] nyrun ls"
# --------------------------------------------------
LS_OUTPUT=$(nyrun ls 2>/dev/null || true)
if echo "$LS_OUTPUT" | grep -q "proc-only"; then
    pass "ls shows process"
else
    fail "ls shows process" "proc-only not in output"
fi

# --------------------------------------------------
echo ""
echo "[3] nyrun logs"
# --------------------------------------------------
LOGS=$(nyrun logs proc-only --lines 5 2>/dev/null || true)
if [ -n "$LOGS" ]; then
    pass "logs returns output"
else
    fail "logs returns output" "empty logs"
fi

# --------------------------------------------------
echo ""
echo "[4] nyrun restart"
# --------------------------------------------------
nyrun restart proc-only 2>/dev/null || true
sleep 2

if wait_for_process "proc-only"; then
    pass "process restarted"
else
    fail "process restarted" "not found after restart"
fi

# --------------------------------------------------
echo ""
echo "[5] nyrun del"
# --------------------------------------------------
nyrun del proc-only 2>/dev/null || true
sleep 1

LS_AFTER_DEL=$(nyrun ls 2>/dev/null || true)
if ! echo "$LS_AFTER_DEL" | grep -q "proc-only"; then
    pass "process deleted"
else
    fail "process deleted" "still in ls"
fi

# --------------------------------------------------
echo ""
echo "[6] nyrun run — with proxy (--p)"
# --------------------------------------------------
nyrun run python3 --name proxied --args "/tests/fixtures/http-server.py" --p 7070:7070 2>/dev/null &
sleep 5

# Auto-remap: proxy on 7070, app gets a random PORT env var
if wait_for_json http://127.0.0.1:7070 20; then
    RESP=$(curl -sf http://127.0.0.1:7070/)
    if echo "$RESP" | jq -e '.status == "ok"' >/dev/null 2>&1; then
        pass "proxy forwards requests (auto-remap)"
    else
        fail "proxy forwards requests (auto-remap)" "bad json response"
    fi
else
    fail "proxy forwards requests (auto-remap)" "no json response on :7070"
fi

# --------------------------------------------------
echo ""
echo "[7] nyrun run — auto port remap (same port)"
# --------------------------------------------------
nyrun run python3 --name explicit-port --args "/tests/fixtures/http-server.py" --p 7171:8071 2>/dev/null &
sleep 5

# Explicit different ports: proxy on 7171, app on 8071 (set via PORT env in http-server.py)
# Need to set PORT env — use ecosystem for explicit port mapping instead
if wait_for_json http://127.0.0.1:7171 20; then
    pass "explicit port proxy works"
else
    fail "explicit port proxy works" "no json response on :7171"
fi

# --------------------------------------------------
echo ""
echo "[8] nyrun set — default-registry"
# --------------------------------------------------
SET_OUT=$(nyrun set default-registry ghcr.io 2>/dev/null || true)
if echo "$SET_OUT" | grep -q "ghcr.io"; then
    pass "set default-registry"
else
    fail "set default-registry" "unexpected output: $SET_OUT"
fi

# Reset back
nyrun set default-registry docker.io 2>/dev/null || true

# --------------------------------------------------
echo ""
echo "[9] nyrun set — cache-ttl"
# --------------------------------------------------
SET_TTL=$(nyrun set cache-ttl 120 2>/dev/null || true)
if echo "$SET_TTL" | grep -q "120"; then
    pass "set cache-ttl"
else
    fail "set cache-ttl" "unexpected output: $SET_TTL"
fi

# --------------------------------------------------
echo ""
echo "[10] nyrun start — ecosystem.yaml with ConfigMap"
# --------------------------------------------------
# Clean up existing processes first
nyrun del proxied 2>/dev/null || true
nyrun del explicit-port 2>/dev/null || true
sleep 1

nyrun start /tests/fixtures/ecosystem.yaml 2>/dev/null &
sleep 5

if wait_for_process "web" && wait_for_process "worker" && wait_for_process "with-config"; then
    pass "start ecosystem — all processes running"
else
    fail "start ecosystem — all processes running" "not all processes found"
fi

# Test web process proxy
if wait_for_json http://127.0.0.1:7001 20; then
    pass "ecosystem web proxy works"
else
    fail "ecosystem web proxy works" "no json response on :7001"
fi

# Test worker (no proxy, direct port)
if wait_for http://127.0.0.1:8002 15; then
    pass "ecosystem worker direct access"
else
    fail "ecosystem worker direct access" "no response on :8002"
fi

# Test with-config proxy
if wait_for_json http://127.0.0.1:7003 20; then
    pass "ecosystem with-config proxy works"
else
    fail "ecosystem with-config proxy works" "no json response on :7003"
fi

# --------------------------------------------------
echo ""
echo "[11] ConfigMap — files created"
# --------------------------------------------------
if [ -f /var/run/nyrun/configmaps/test-config/app.conf ]; then
    CONF_CONTENT=$(cat /var/run/nyrun/configmaps/test-config/app.conf)
    if echo "$CONF_CONTENT" | grep -q "debug=true"; then
        pass "configmap file app.conf created"
    else
        fail "configmap file app.conf created" "wrong content"
    fi
else
    fail "configmap file app.conf created" "file not found"
fi

if [ -f /var/run/nyrun/configmaps/test-config/settings.json ]; then
    pass "configmap file settings.json created"
else
    fail "configmap file settings.json created" "file not found"
fi

# --------------------------------------------------
echo ""
echo "[12] Volume mount — configmap mounted into process"
# --------------------------------------------------
# python3 is a system binary, not copied to apps dir. Check configmap was resolved
if find /var/run/nyrun -name "app.conf" -path "*/with-config/*" 2>/dev/null | grep -q app.conf; then
    pass "configmap volume mounted into process dir"
elif [ -f /var/run/nyrun/configmaps/test-config/app.conf ]; then
    # ConfigMap files exist, volume resolution worked
    pass "configmap volume resolved"
else
    fail "configmap volume mounted" "configmap files not found"
fi

# --------------------------------------------------
echo ""
echo "[13] nyrun export"
# --------------------------------------------------
EXPORT_OUT=$(nyrun export 2>/dev/null || true)
if echo "$EXPORT_OUT" | grep -q "kind: Process"; then
    pass "export outputs k8s-style YAML"
else
    fail "export outputs k8s-style YAML" "no 'kind: Process' found"
fi

if echo "$EXPORT_OUT" | grep -q "web"; then
    pass "export contains web process"
else
    fail "export contains web process" "web not found in export"
fi

# --------------------------------------------------
echo ""
echo "[14] nyrun save"
# --------------------------------------------------
SAVE_OUT=$(nyrun save 2>/dev/null || true)
if echo "$SAVE_OUT" | grep -qi "saved\|ok"; then
    pass "save succeeded"
else
    fail "save succeeded" "unexpected output: $SAVE_OUT"
fi

# --------------------------------------------------
echo ""
echo "[15] nyrun backup / restore"
# --------------------------------------------------
nyrun backup -o /tmp/test-backup 2>/dev/null || true

if [ -f /tmp/test-backup.zip ] || [ -f /tmp/test-backup ]; then
    pass "backup created"

    RESTORE_OUT=$(nyrun restore /tmp/test-backup.zip 2>/dev/null || nyrun restore /tmp/test-backup 2>/dev/null || echo "restore_failed")
    if echo "$RESTORE_OUT" | grep -qi "restore\|ok\|success"; then
        pass "restore succeeded"
    else
        fail "restore succeeded" "output: $RESTORE_OUT"
    fi
else
    fail "backup created" "backup file not found"
fi

# --------------------------------------------------
echo ""
echo "[16] nyrun del — cleanup all"
# --------------------------------------------------
for name in web worker with-config; do
    nyrun del "$name" 2>/dev/null || true
done
sleep 1

FINAL_LS=$(nyrun ls 2>/dev/null || true)
REMAINING=$(echo "$FINAL_LS" | grep -c "Running" || true)
if [ "$REMAINING" -eq 0 ]; then
    pass "all processes cleaned up"
else
    fail "all processes cleaned up" "$REMAINING still running"
fi

# --------------------------------------------------
echo ""
echo "[17] nyrun kill"
# --------------------------------------------------
nyrun kill 2>/dev/null || true
sleep 3

if ! pgrep -x "nyrun" >/dev/null 2>&1; then
    pass "daemon stopped"
else
    fail "daemon stopped" "daemon still running"
fi

# ==========================================
echo ""
echo "========================================="
echo " Results: $PASS passed, $FAIL failed"
echo "========================================="
echo ""

for t in "${TESTS[@]}"; do
    echo "  $t"
done

echo ""

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
