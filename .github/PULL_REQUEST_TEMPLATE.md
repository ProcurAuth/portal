## What changes

Describe the change in a few sentences.

## Why

Give the reason. Link the issue.

## Checks

- [ ] I ran `tools/dev/lint.sh`.
- [ ] I ran `tools/dev/test.sh --workspace`. `docs/development.md` tells how.
- [ ] The documents change with the code, and I ran `python3 -I scripts/check-docs.py`.
- [ ] A change of a document follows ASD-STE100: `python3 -I tools/dev/ste-check.py` finds no problem in the files that I changed.
- [ ] A new configuration key is in the table of keys (`src/config/keys.rs`), and I made the reference in `docs/configuration.md` again.
- [ ] I updated `CHANGELOG.md` under "Unreleased".
- [ ] The files have no secret, no real host name, no real address and no personal data.
