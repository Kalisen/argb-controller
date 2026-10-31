# argb-controller

A Windows service that centrally drives the RGB lighting of a PC and, later, of the
Govee lights in the same room. One configuration file decides what every device shows
while the machine is awake and what it shows while the machine sleeps.

## Status

Early development. The service connects to a local OpenRGB SDK server, maps its
controllers to configured aliases and applies the configured lighting; `argb devices list`
shows that mapping. Sleep handling, synchronized pulsing and the installer are still to
come. See `docs/requirements.md` for the scope and `docs/architecture.md` for the design.

Running it today needs [OpenRGB](https://openrgb.org) 1.0 (the PawnIO build) with its SDK
server enabled on the loopback interface.

## Phases

| Phase | Scope |
|---|---|
| 1 | In-machine hardware through [OpenRGB](https://openrgb.org): motherboard and its ARGB headers (fans, AIO), DDR5 RAM, GPU |
| 2 | Govee devices over Bluetooth LE: Glide Hexa panels (H6061), floor lamp (H6076) |
| 2.5 | Security review of everything implemented so far; every finding is fixed before Phase 3 |
| 3 | Samsung Odyssey Neo G9 rear lighting, pending a feasible control path |

## Development conventions

- Rust, latest stable toolchain.
- New work happens in a `feature/...`, `fix/...` or `maintain/...` branch checked out as a
  git worktree under `.worktrees/` (ignored by git), so `main` stays available for experiments.
- Commit messages follow the template in `docs/contributing.md`.

## License

MIT. See [LICENSE](LICENSE).
