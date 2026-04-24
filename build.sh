#!/usr/bin/env bash
set -euo pipefail

rm -f -v src-tauri/target/x86_64-unknown-linux-gnu/release/bundle/deb/CC*deb

if [ ! -d node_modules ]; then
  echo "node_modules 不存在，请先运行: pnpm install" >&2
  exit 1
fi

pnpm exec tauri build \
  --bundles deb \
  --target x86_64-unknown-linux-gnu \
  --config '{"bundle":{"createUpdaterArtifacts":false}}'

deb_path="$(find "src-tauri/target/x86_64-unknown-linux-gnu/release/bundle/deb" -maxdepth 1 -type f -name '*.deb' | head -n 1)"

if [ -z "${deb_path}" ]; then
  echo "构建完成，但未找到 .deb 产物" >&2
  exit 1
fi

version="$(basename "${deb_path}" | sed -E 's/^.*_([0-9][^_]*)_amd64\.deb$/\1/')"
output_path="./CC-Switch_${version}_amd64.deb"
cp -f "${deb_path}" "${output_path}"

echo "${output_path}"
