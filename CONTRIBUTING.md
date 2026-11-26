# Contributing

1. Read [AGENTS.md](AGENTS.md). Its rules apply to every contribution.
2. Add protocol facts only with a cited source and verification level in
   `docs/protocol-compliance.md`.
3. Hardware results must include scanner model, firmware version, network
   setup and the exact test command. Add them to `docs/hardware-testing.md`.
4. Run the quality gates listed in `AGENTS.md` before opening a pull
   request. For non-trivial changes, use the review workflow described
   there (`.claude/skills/radio-workflow`), or follow the same checklist by
   hand: threat sketch, tests for malformed input and authorization, and
   updated compliance docs.
5. Keep `docs/nist-800-53-controls.md` true: change a control's status only
   with a cited implementation and test.
6. Contributions are licensed under Apache-2.0. Sign off your commits
   (`git commit -s`, DCO).

Community projects listed in `docs/protocol-compliance.md` may be consulted
for protocol facts. **Do not copy their code**, because several are
GPL-licensed.
