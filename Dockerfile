ARG BASE_IMAGE=xiechengqi/ivnc:latest
FROM ${BASE_IMAGE}

# Build arg names the deb file expected to be present in the build context.
# CI (release-latest.yml) places it there via download-artifact before docker build.
# Local manual build:
#   wget -O cc-switch-linux-amd64-ubuntu22-latest.deb \
#     https://github.com/Xiechengqi/cc-switch/releases/download/latest/cc-switch-linux-amd64-ubuntu22-latest.deb
#   docker build -f Dockerfile -t ghcr.io/xiechengqi/cc-switch:latest .
# Runtime:
#   docker run -d \
#     -v ~/.config/ivnc:/root/.config/ivnc \
#     ghcr.io/xiechengqi/cc-switch:latest
ARG DEB_NAME=cc-switch-linux-amd64-ubuntu22-latest.deb

COPY ${DEB_NAME} /tmp/${DEB_NAME}

RUN set -eux; \
    apt-get update; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        curl \
        fontconfig \
        fonts-noto-color-emoji \
        sqlite3 \
        "/tmp/${DEB_NAME}"; \
    fc-cache -fv; \
    rm -f "/tmp/${DEB_NAME}"; \
    rm -rf /var/lib/apt/lists/*

COPY docker-entrypoint.cc-switch.sh /usr/local/bin/docker-entrypoint.cc-switch.sh

RUN chmod +x /usr/local/bin/docker-entrypoint.cc-switch.sh

VOLUME ["/root/.config/ivnc"]

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.cc-switch.sh", "--config", "/etc/ivnc.toml", "--"]
CMD ["sleep", "infinity"]
