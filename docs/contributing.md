# Contributing

## Branches and worktrees

New work happens on a branch named for its purpose, checked out as a git
worktree under `.worktrees/` so `main` stays available for experiments:

```
git worktree add -b feature/issue-12-sleep-transitions .worktrees/feature-issue-12-sleep-transitions main
```

Branch names use lower-case letters, digits, hyphens and dots only, with one
of these prefixes:

| Prefix | Use |
|---|---|
| `feature/` | New capability |
| `fix/` | Bug fix |
| `maintain/` | Dependencies, docs, tooling |

Include the issue number when there is one: `fix/issue-34-ram-not-detected`.

## Commit messages

```
Brief description of the purpose of the change (1 or 2 sentences).

# Context
- If it's a bug, what was the issue?
- If it's a feature, what capability is being implemented?
- If it's maintenance, what is the scope of the maintenance.
Link to the related issue when there is one.

# Approach
A brief description of the intent and structure of the changes made.
List any key components of the change.
If applicable, list any downstream consequences.
```

When an AI model wrote or co-wrote the change, end the message with
`AI Model: <model name>`. Pull request descriptions follow the same template
without that line.

## Code conventions

- Values that never change at runtime are constants, placed on the type they
  describe. Related constants are enums.
- Business logic lives as deep as it can: in `argb-domain` or `argb-engine`,
  never in the service or CLI binaries.
- Log messages are not constants, though they may interpolate constants.
- `unsafe` is forbidden in project crates.
- Run `cargo fmt`, `cargo clippy --all-targets` and `cargo test` before
  committing, and `cargo audit` (from `cargo install cargo-audit`) when
  `Cargo.lock` changes: a dependency with a published advisory is fixed by
  bumping it, never waived.

## Tests

Tests express requirements and demonstrate behavior. Every valid scenario is
covered once; impossible inputs are not. Backends are tested against fakes
(see the OpenRGB client tests for an in-process fake server). Real hardware is
exercised through the CLI, never from the test suite.
