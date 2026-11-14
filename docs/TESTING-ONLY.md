# Removing the testing-only code

The "Skip installation" choice exists only so the system can be tried without a disk. Everything is marked `TESTING-ONLY` in the code:

    grep -rn "TESTING-ONLY" ui/ scripts/ .github/

To remove it: delete those pieces (installer `SKIP_NAME`/`skip_selected`/`Outcome::SetupDone`, `install::setup_in_memory`/`MEMORY_STEPS`,
`EXIT_SETUP_DONE` in the supervisor, the `exec85.testing` kernel flag, the `exec85-testing.iso` build in `scripts/build-image.sh`,
the `-testing` flavour in `release.yml`, and the `--testing` scenario in `scripts/boot-test.py`), then stop publishing `-testing` releases.

**Since v0.9.3** the testing ISO is no longer published as a release: it is built and used only by the automatic tests in CI (a quick way to try
every app without a disk). `release.yml` refuses `-testing` tags. Try Exec85 yourself in VirtualBox with the normal ISO instead.
