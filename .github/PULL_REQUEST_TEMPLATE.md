## What this change does

<!-- Say what changes and why. Link the issue if there is one. -->

## How you checked it

- [ ] `tools/dev/lint.sh` passes.
- [ ] `tools/dev/test.sh` passes (see [Development](../docs/development.md)).
- [ ] The documentation changes with the code. `python3 tools/docs/build_site.py --check` and `python3 tools/docs/check_links.py` pass.
- [ ] The documents follow ASD-STE100: `python3 tools/dev/ste-check.py` reports no problem for the files that you changed.
- [ ] A new configuration key is in the table of keys (`src/config/keys.rs`) and the reference in `docs/configuration.md` is generated again.
- [ ] The change has a line in `CHANGELOG.md` under Unreleased.
- [ ] The change holds no secret, no real host name and no personal data.

When you send this change, you agree that it is licensed under `MIT OR Apache-2.0`.
