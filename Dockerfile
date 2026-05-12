ARG BASE_IMAGE=ghcr.io/xiechengqi/ivnc:latest
FROM ${BASE_IMAGE}

# Build arg names the deb file expected to be present in the build context.
# CI (release-latest.yml) places it there via download-artifact before docker build.
# Local manual build:
#   wget -O cc-switch-linux-amd64-ubuntu22-latest.deb \
#     https://github.com/Xiechengqi/cc-switch/releases/download/latest/cc-switch-linux-amd64-ubuntu22-latest.deb
#   docker build -f Dockerfile -t ghcr.io/xiechengqi/cc-switch:latest .
# Runtime:
#   docker run -d \
#     -v ~/.cc-switch:/root/.cc-switch \
#     ghcr.io/xiechengqi/cc-switch:latest
ARG DEB_NAME=cc-switch-linux-amd64-ubuntu22-latest.deb

COPY ${DEB_NAME} /tmp/${DEB_NAME}

RUN set -eux; \
    apt-get update; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends curl "/tmp/${DEB_NAME}"; \
    curl -SsL https://github.com/YUxiangLuo/miao/releases/download/v0.18.4/miao-rust-linux-amd64 -o /usr/local/bin/miao; \
    chmod +x /usr/local/bin/miao; \
    rm -f "/tmp/${DEB_NAME}"; \
    rm -rf /var/lib/apt/lists/*; \
    mkdir -p /root/.config/ivnc && \
    sqlite3 /root/.config/ivnc/apps.db "\
CREATE TABLE IF NOT EXISTS apps (\
    id TEXT PRIMARY KEY,\
    name TEXT NOT NULL UNIQUE,\
    url TEXT,\
    mode TEXT,\
    dark_mode INTEGER DEFAULT 0,\
    autostart INTEGER DEFAULT 0,\
    show_nav INTEGER DEFAULT 0,\
    created_at TEXT NOT NULL,\
    app_type TEXT DEFAULT 'webapp',\
    exec_command TEXT,\
    env_vars TEXT,\
    remote_debugging_port INTEGER,\
    proxy_server TEXT,\
    launch_command TEXT,\
    launch_env_vars TEXT,\
    launch_cwd TEXT,\
    launch_wait_url TEXT,\
    launch_wait_timeout_secs INTEGER\
);\
INSERT OR IGNORE INTO apps \
    (id, name, app_type, url, mode, autostart, show_nav, exec_command, env_vars, created_at, remote_debugging_port, proxy_server, launch_command, launch_env_vars, launch_cwd, launch_wait_url, launch_wait_timeout_secs) \
VALUES \
    ('preset-cc-switch', 'cc-switch', 'desktop', NULL, NULL, 1, 0, 'cc-switch', '', '2026-04-23T00:00:00Z', NULL, NULL, NULL, NULL, NULL, NULL, NULL),\
    ('preset-miao', 'Miao', 'webapp', 'http://localhost:6161', 'webview', 1, 0, '', '', '2026-05-12T00:00:00Z', NULL, NULL, 'miao', '', NULL, NULL, NULL);\
"

VOLUME ["/root/.cc-switch"]
