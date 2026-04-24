# Share Router Rename Notes

`portr-rs` has been renamed to `cc-switch-router`.

## What changed in cc-switch

- User-facing copy now refers to `cc-switch-router`
- Share router settings now use:
  - backend field: `share_router_domain`
  - frontend field: `shareRouterDomain`
- Older fields remain readable for compatibility:
  - backend: `portr_domain`
  - frontend: `portrDomain`
- Internal router endpoints now prefer `/_share-router/*`
- Internal router headers now prefer `X-Share-Router-*`

## Compatibility behavior

This tree still supports:

- loading old settings that contain `portr_domain`
- frontend state objects that still contain `portrDomain`
- older router implementations that only expose `/_portr/*`
- older router implementations that only understand `X-Portr-*`

## Current preferred names

Use these names in all new code:

- `share_router_domain`
- `shareRouterDomain`
- `/_share-router/health`
- `/_share-router/request-logs`
- `/_share-router/share-runtime`
- `X-Share-Router-Probe`
- `X-Share-Router-Ping-Request`
- `X-Share-Router-Error`
- `X-Share-Router-Error-Reason`

## Cleanup later

Do not remove the `portr_*` compatibility layer until:

1. the deployed router fleet has been upgraded
2. desktop clients in the field have rolled forward
3. old persisted settings have had time to rewrite in the new shape
