#!/usr/bin/env bash
set -euo pipefail

log() {
    echo "[cc-switch-entrypoint] $*" >&2
}

fail() {
    log "ERROR: $*"
    exit 1
}

ivnc_config_dir="${IVNC_CONFIG_DIR:-${XDG_CONFIG_HOME:-/root/.config}/ivnc}"
cc_switch_data_dir="${CC_SWITCH_DATA_DIR:-$ivnc_config_dir/cc-switch}"
apps_db="$ivnc_config_dir/apps.db"
root_cc_switch="/root/.cc-switch"

mkdir -p "$cc_switch_data_dir" || fail "failed to create cc-switch data dir: $cc_switch_data_dir"

if [[ -L "$root_cc_switch" || ! -e "$root_cc_switch" ]]; then
    rm -f "$root_cc_switch"
    ln -s "$cc_switch_data_dir" "$root_cc_switch"
elif [[ -d "$root_cc_switch" ]]; then
    if [[ -z "$(find "$root_cc_switch" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null)" ]]; then
        rmdir "$root_cc_switch"
        ln -s "$cc_switch_data_dir" "$root_cc_switch"
    else
        log "$root_cc_switch exists and is not empty; leaving it unchanged"
    fi
else
    log "$root_cc_switch exists and is not a symlink or directory; leaving it unchanged"
fi

command -v sqlite3 >/dev/null 2>&1 || fail "sqlite3 is required to seed iVNC apps"
mkdir -p "$ivnc_config_dir" || fail "failed to create iVNC config dir: $ivnc_config_dir"

cat > /usr/local/bin/cc-switch-gated-start <<'SH'
#!/usr/bin/env bash
set -euo pipefail

mkdir -p /root/.cc-switch

echo "[cc-switch] waiting 5 seconds before network location check" >&2
sleep 5

can_start=0
for i in 1 2 3; do
    location="$(curl -SsL --max-time 5 3.0.3.0 2>/dev/null | grep 'location' || true)"
    if [[ -n "$location" ]]; then
        echo "[cc-switch] network location check $i: $location" >&2
        if [[ "$location" != *"中国"* ]]; then
            can_start=1
            break
        fi
    else
        echo "[cc-switch] network location check $i: empty result" >&2
    fi
done

if [[ "$can_start" -ne 1 ]]; then
    echo "[cc-switch] not starting: all 3 location checks were empty or still matched 中国" >&2
    exit 1
fi

echo "[cc-switch] network location check passed; starting cc-switch" >&2
exec cc-switch
SH
chmod +x /usr/local/bin/cc-switch-gated-start

sqlite3 "$apps_db" <<'SQL'
CREATE TABLE IF NOT EXISTS apps (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    url TEXT,
    autostart INTEGER DEFAULT 0,
    created_at TEXT NOT NULL,
    app_type TEXT DEFAULT 'background',
    exec_command TEXT,
    env_vars TEXT,
    launch_command TEXT,
    launch_env_vars TEXT,
    launch_cwd TEXT,
    launch_wait_timeout_secs INTEGER
);
SQL

ensure_column() {
    local column="$1"
    local definition="$2"
    if ! sqlite3 "$apps_db" "SELECT 1 FROM pragma_table_info('apps') WHERE name = '$column';" | grep -qx "1"; then
        sqlite3 "$apps_db" "ALTER TABLE apps ADD COLUMN $definition;"
    fi
}

ensure_column "url" "url TEXT"
ensure_column "autostart" "autostart INTEGER DEFAULT 0"
ensure_column "created_at" "created_at TEXT"
ensure_column "app_type" "app_type TEXT DEFAULT 'background'"
ensure_column "exec_command" "exec_command TEXT"
ensure_column "env_vars" "env_vars TEXT"
ensure_column "launch_command" "launch_command TEXT"
ensure_column "launch_env_vars" "launch_env_vars TEXT"
ensure_column "launch_cwd" "launch_cwd TEXT"
ensure_column "launch_wait_timeout_secs" "launch_wait_timeout_secs INTEGER"

sqlite3 "$apps_db" <<'SQL'
INSERT OR IGNORE INTO apps
    (id, name, app_type, url, autostart, exec_command, env_vars, created_at, launch_command, launch_env_vars, launch_cwd, launch_wait_timeout_secs)
VALUES
    ('preset-cc-switch', 'cc-switch', 'desktop', NULL, 1, '/usr/local/bin/cc-switch-gated-start', '', '2026-04-23T00:00:00Z', NULL, NULL, NULL, NULL);
UPDATE apps
SET app_type = 'desktop',
    autostart = 1,
    exec_command = '/usr/local/bin/cc-switch-gated-start'
WHERE id = 'preset-cc-switch' OR name = 'cc-switch';
SQL

log "ensured cc-switch app preset and data dir under $ivnc_config_dir"

original_entrypoint="/usr/local/bin/docker-entrypoint.sh"
[[ -x "$original_entrypoint" ]] || fail "iVNC entrypoint is missing or not executable: $original_entrypoint"

exec "$original_entrypoint" "$@"
