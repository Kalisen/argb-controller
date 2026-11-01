# argb-controller

A Windows service that centrally drives the RGB lighting of a PC and of the Govee
lights in the same room. One configuration file decides what every device shows while
the machine is awake and what it shows while the machine sleeps.

## Status

Phase 1 is complete and runs as an installed service on the author's machine. The
service connects to a local OpenRGB SDK server, maps its controllers to configured aliases
and applies the configured lighting: a static color or a software pulse synchronized across
devices while Windows runs, device-native effects saved to device memory for sleep and
boot. It survives suspend/resume cycles and OpenRGB's hardware detection at boot.
`argb devices scan` adds configuration entries for new hardware and `argb service install`
installs or upgrades the service.

Phase 2 adds the Govee lights: the service drives the Glide Hexa panels (H6061) and the
Lyra floor lamp (H6076) directly over Bluetooth LE, including the encrypted channel the
lamp uses, from the same configuration file and the same power states. `argb devices scan`
hears the lights and adds them; a light that is off or out of range is retried while the
PC hardware keeps being driven. See `docs/requirements.md` for the scope,
`docs/architecture.md` for the design and `docs/phase2-govee-ble.md` for the protocol.

Phase 2.1 covers the USB peripherals. The Logitech G203 mouse was already visible through
OpenRGB; its static color turned out to be an OpenRGB driver defect, worked around in the
adapter and reported upstream. The Cloud Nine C959 keyboard is driven directly over USB HID
with the protocol of its vendor application, captured and documented in
`docs/phase2-1-peripherals.md`: static color with true brightness while awake, the keyboard's
own breathing effect for sleep, state kept in its flash.

Running it needs [OpenRGB](https://openrgb.org) 1.1 or later (the PawnIO build) with its SDK
server enabled on the loopback interface, and a Bluetooth LE adapter for the Govee lights.

## Install

From an elevated prompt, after `cargo build --release`:

```
target\release\argb.exe service install
target\release\argb.exe service check
```

This copies the binaries to `%ProgramFiles%\argb-controller`, writes a starter
configuration to `%ProgramData%\argb-controller\config.toml` (administrators only), and
registers and starts the `argb-controller` service. Then describe the hardware:

```
argb devices scan
argb config set-default --state active --mode static --color "#40E0D0"
```

The service picks up every configuration change within a second and logs to
`%ProgramData%\argb-controller\logs`. `argb service uninstall`
removes the registration and leaves the files.

## Phases

| Phase | Scope | Status |
|---|---|---|
| 1 | In-machine hardware through [OpenRGB](https://openrgb.org): motherboard and its ARGB headers (fans, AIO), DDR5 RAM, GPU | done |
| 2 | Govee devices over Bluetooth LE: Glide Hexa panels (H6061), floor lamp (H6076) | done |
| 2.1 | USB peripherals: Logitech G203 Lightsync mouse (through OpenRGB), Cloud Nine C959 ErgoTKL keyboard (own Sonix HID backend) | in progress: sleep and boot behavior of the keyboard to verify |
| 2.5 | Security review of everything implemented so far; every finding is fixed before Phase 3 | done: see "Security" in `docs/architecture.md` |
| 3 | Samsung Odyssey Neo G9 rear lighting, pending a feasible control path | |

## Development conventions

- Rust, latest stable toolchain.
- New work happens in a `feature/...`, `fix/...` or `maintain/...` branch checked out as a
  git worktree under `.worktrees/` (ignored by git), so `main` stays available for experiments.
- Commit messages follow the template in `docs/contributing.md`.

## License

MIT. See [LICENSE](LICENSE).
