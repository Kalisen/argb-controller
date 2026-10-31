# Requirements

A Windows service that centrally drives the RGB lighting of one PC and, later,
of the Govee lights in the same room. The user edits one configuration and the
service makes every device follow it, whether the machine is awake or asleep.

## Phases

| Phase | Scope | Exit criterion |
|---|---|---|
| 1 | In-machine hardware through OpenRGB | Every Phase 1 device follows the configuration in both power states and shows the configured boot lighting during POST |
| 2 | Govee Glide Hexa (H6061) and Govee floor lamp (H6076) over Bluetooth LE | Both devices follow the configuration in both power states |
| 2.5 | Security review of all code written so far | Every finding is fixed and the fixes are covered by tests |
| 3 | Samsung Odyssey Neo G9 57" (G95NC) rear lighting | A feasible control path exists and the monitor follows the configuration, or the device is documented as unsupported |

## Phase 1 hardware

| Device | Connection | Controlled through |
|---|---|---|
| Gigabyte Z790 AORUS Elite X WIFI7 | USB HID (ITE RGB Fusion 2 controller) | OpenRGB "Gigabyte RGB Fusion 2 USB" |
| Phanteks D30-120 DRGB fans (3) | Board ARGB header | Same controller as the board |
| Arctic Liquid Freezer III 360 A-RGB | Board ARGB header | Same controller as the board |
| Corsair Vengeance RGB DDR5 (2 x 32 GB) | SMBus | OpenRGB "Corsair DRAM" |
| ASUS GeForce RTX 4080 TUF Gaming OC | I2C over the NVIDIA driver | OpenRGB "ASUS Aura GPU" |

## Functional requirements

### States

- **R1** The service distinguishes two power states: `active` (Windows running) and `sleep` (a Windows sleep state).
- **R2** Each state has a default lighting mode that applies to every device without an override.
- **R3** Any device may override the default for either state independently.
- **R4** On service start and on resume from sleep the service applies the `active` lighting.
- **R5** On the Windows suspend notification the service applies the `sleep` lighting before the machine suspends.
- **R6** Because no software runs while the machine sleeps, the `sleep` lighting must be realized with device-native modes that keep running on their own.

### Modes

- **R7** `static`: one color at a configured intensity. Intensity means the amount of emitted light; devices with a hardware brightness control use it, others scale the color.
- **R8** `pulse`: one color breathing between off and full with a configured cycle period.
- **R9** While `active`, pulses on all devices are synchronized from one clock so they rise and fall together.
- **R10** While asleep, each device runs its own hardware breathing effect with the closest available period. Drift between devices in this state is accepted.

### Boot lighting

- **R11** The lighting shown during POST and boot, before Windows runs, is whatever each device holds in its own memory. The service writes one state's lighting there; which state is configurable and defaults to `active`.
- **R12** The CLI offers a repeatable persistence probe: write a distinct color per device with the save command, then the user cold boots and reports what each device shows.

### Configuration

- **R13** Configuration is one TOML file in the machine-wide program data folder.
- **R14** A command-line tool reads and edits the file. All edits are serialized through a lock and written atomically, so scripts can drive it without risking a corrupt file.
- **R15** The service picks up configuration changes without a restart.
- **R16** The file rejects unknown keys and out-of-range values with a clear message naming the offending entry.
- **R17** A CLI scan command detects hardware that the backends report but the configuration does not list, appends a default entry for each such device, and the running service applies the updated configuration. See [issue 2](https://github.com/Kalisen/argb-controller/issues/2).

### Service

- **R18** The program runs as a Windows Service that starts automatically at boot, before any user logs in.
- **R19** The same binary can run in the foreground for development.
- **R20** Device failures are logged and do not stop the service or the other devices.

## Non-functional requirements

- **N1** Rust, latest stable toolchain, `unsafe` forbidden in project crates.
- **N2** OpenRGB (the PawnIO build) runs headless as an SDK server on the loopback interface. It is the only program allowed to touch the Phase 1 controllers; vendor RGB software is disabled or removed.
- **N3** The OpenRGB SDK is unauthenticated, so the service only ever connects to it on the loopback interface, and the configuration schema documents that constraint.
- **N4** The repository is public. No credentials, machine identifiers, personal paths or Bluetooth pairing material are committed.
- **N5** Unit tests cover every valid scenario of the domain, configuration, engine and protocol encoding. Backends are tested against fakes; real hardware is exercised through the CLI, not the test suite.

## Out of scope for Phase 1

- Per-LED or per-zone effects. Every device shows one color.
- Game or audio reactive lighting.
- A graphical configuration UI.
- Devices outside the room, or anything that needs Wi-Fi.
