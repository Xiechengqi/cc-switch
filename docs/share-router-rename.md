# Share Router Rename Notes

`portr-rs` has been renamed to `cc-switch-router`.

## What changed in cc-switch

- User-facing copy now refers to `cc-switch-router`
- Share router settings now use:
  - backend field: `share_router_domain`
  - frontend field: `shareRouterDomain`
- Internal router endpoints now prefer `/_share-router/*`
- Internal router headers now prefer `X-Share-Router-*`

## Compatibility behavior

The `portr_*`, `portrDomain`, `/_portr/*`, and `X-Portr-*` compatibility layer has
been removed. Use the current names below.

## Current preferred names

Use these names in all new code:

- `share_router_domain`
- `shareRouterDomain`
- `/_share-router/health`
- `/_share-router/request-logs`
- `/_share-router/share-runtime`
- `X-Share-Router-Probe`
- `X-Share-Router-Error`
- `X-Share-Router-Error-Reason`
