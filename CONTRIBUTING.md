# Contributing

Thank you for your interest in VCam CLI. This project is maintained by a single person with limited time for review. The rules below exist so that contributions can be reviewed and merged reliably within that time. Pull requests that do not follow them may be closed regardless of their content.

## Discuss large changes in an issue first

Unless your change is a bug fix, a typo fix in the documentation, or a minor suggestion, open an issue and agree on the approach before writing a pull request. The following always require prior discussion:

- New commands, flags, or MCP tools, or changes to how parameters are read or results are printed
- Changes to the exit codes or the JSON output, which scripts depend on
- Changes to dependencies or the supported platforms
- Changes to the build, the GitHub Actions workflows or the release process

VCam CLI builds its commands from the API specification the running VCam serves. Features that VCam itself should offer belong to [VCam](https://github.com/vcamapp/app/issues), not here.

If a large pull request arrives without prior discussion and its design does not match the direction of the project, none of it can be accepted. Reach agreement in an issue before you start implementing.

## One pull request, one purpose

- Solve exactly one problem per pull request. Put "while I was at it" changes in separate pull requests
- Keep the change under 300 lines as a guideline, and under 500 lines at most, not counting tests. If it has to be larger, propose how to split it in an issue first
- Pull requests that bundle several changes, bring over the state of a fork as is, or resolve several issues at once will not be accepted
- Keep refactoring separate from feature work

## Checklist before opening a pull request

- [ ] The description links the related issue (for example `Closes #123`)
- [ ] The change has a single, focused purpose
- [ ] The branch is rebased on the latest `main`
- [ ] `cargo test` passes
- [ ] `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` pass
- [ ] Tests covering the changed behavior are added or updated
- [ ] No unrelated files are included (formatting-only diffs, generated files, and so on)
- [ ] The README is updated if what users see or do changed

## Code

- Match the style, naming, and comment density of the surrounding code
- Write comments only where they explain why something is done. Comments that describe what the code does, or what was changed, are not needed
- Do not leave commented-out code behind
- Do not hard-code the API's methods or parameters. Read them from the specification, so that VCam CLI keeps working as the API grows. Only what the specification cannot express, such as uploads, is written by hand
- Build output goes to `.build/`, which `.cargo/config.toml` sets as the target directory

## Tests

```sh
cargo test
```

The tests run against a fake WebSocket server and the copy of the API specification in `spec/openrpc.json`, so they need no VCam. Refresh the copy with `scripts/update-spec.sh` when the API changes.

People keep using the VCam version they bought, so VCam CLI has to work with every released version of the VCam API. `spec/history/<version>.json` holds the specification of each released version, and the tests build the tools from each of them and check that no version removes or changes what an older one offers. When a VCam release ships a new API version, add its specification there.

To try a change against VCam, turn on the VCam API in its settings and run the command, or the MCP server with an MCP client. Work in a copy of a scene (`vcam scene.duplicate`), since changes made through the API are saved to the scene.

GitHub Actions checks the format and the lints, and runs the tests on Linux, macOS, and Windows. Pull requests with failing checks will not be reviewed.

## Changes made with AI tools

Using AI tools is not prohibited, but the following applies:

- Before submitting, read, understand, and verify every line of the change yourself. Do not submit changes you cannot explain
- Do not submit large AI-generated changes as they are. The size and scope limits above apply unchanged
- Claims such as "tested" or "verified" in the pull request description must describe what you actually did yourself
- If you used an AI tool, mention it in the pull request description or in a commit trailer. This is not required, but it helps the review

## Review

- Review can take time. Please refrain from posting reminders while waiting for a reply
- A change may be declined because it does not fit the direction of the project, or because its maintenance cost outweighs its benefit
- Pull requests that receive no reply or update for an extended period will be closed

## Releases

Releases are built by [dist](https://github.com/axodotdev/cargo-dist) from `dist-workspace.toml`. To release:

1. Set the version in `Cargo.toml` and commit it to the default branch.
2. Tag the commit `v<version>`, such as `v0.1.0`, and push the tag.

The Release workflow builds VCam CLI for macOS, Linux, and Windows, attaches the archives and the installers to a GitHub release, and updates the `vcam-cli` formula in [vcamapp/homebrew-tap](https://github.com/vcamapp/homebrew-tap). It needs a `HOMEBREW_TAP_TOKEN` secret that can push to the tap. Run `dist generate` after changing `dist-workspace.toml`, and check what a release will contain with `dist plan`.

## License

By contributing, you agree that your contribution is released under the project's [license](./LICENSE).
