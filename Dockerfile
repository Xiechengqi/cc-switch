ARG BASE_IMAGE=ghcr.io/xiechengqi/ivnc:latest
FROM ${BASE_IMAGE}

# Example:
#   docker build -f Dockerfile -t ghcr.io/xiechengqi/cc-switch:latest .
# Runtime:
#   docker run -d \
#     -v ~/.cc-switch:/root/.cc-switch \
#     ghcr.io/xiechengqi/cc-switch:latest

ARG RELEASE_REPO=Xiechengqi/cc-switch
ARG RELEASE_TAG=latest
ARG DEB_NAME=cc-switch-linux-amd64-ubuntu22-latest.deb

RUN set -eux; \
    apt-get update; \
    wget "https://github.com/${RELEASE_REPO}/releases/download/${RELEASE_TAG}/${DEB_NAME}" -O "/tmp/${DEB_NAME}"; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "/tmp/${DEB_NAME}"; \
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
    proxy_server TEXT\
);\
INSERT OR IGNORE INTO apps \
    (id, name, app_type, url, mode, autostart, show_nav, exec_command, env_vars, created_at, remote_debugging_port, proxy_server) \
VALUES \
    ('preset-cc-switch', 'cc-switch', 'desktop', NULL, NULL, 1, 0, 'cc-switch', '', '2026-04-23T00:00:00Z', NULL, NULL);\
"

VOLUME ["/root/.cc-switch"]
