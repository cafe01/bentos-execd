# Next Steps for john-execd-05

## M6 and M7 are done. Full exec stack shipped.

M6: OpenRC service file + rootfs integration. Committed (a5956b7).
M7: Swift VMM exec endpoints. Committed (e41e8a6).

## What's next (not yet scoped)
- Dart CLI integration: `bentos vm exec`, `bentos vm shell`
- Console (Flutter) integration for exec
- M5.5 vsock integration test (needs real BentOS VM environment)
- End-to-end smoke test: host CLI -> VMM -> vsock -> bentos-execd -> guest process
