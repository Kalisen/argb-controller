# Phase 2.1: USB peripherals

Working notes for the two USB peripherals: what each one exposes, how it is
driven, and what remains.

## Logitech G203 Lightsync (mouse)

| Fact | Value |
|---|---|
| USB id | `046D:C092` (the G102 Lightsync shares it; OpenRGB names both "Logitech G203 Lightsync") |
| Control path | OpenRGB controller "Logitech G203 Lightsync", HID++ feature reports on interface 1 |
| OpenRGB modes | Direct (per-LED, 3 LEDs), Off, Static, Cycle, Breathing, Wave, Colormixing |
| Save to device | none in OpenRGB 1.1 (`@save :robot:`); OpenRGB does not touch the onboard profile |
| Other software seen | Logitech G HUB (agent, tray, updater) and `logi_lamparray_service`; Windows Dynamic Lighting off |

### Why the static color did not apply

The OpenRGB driver `RGBController_LogitechG203L` builds its `Static` and
`Breathing` modes with `colors.resize(1)` but never sets `colors_min` and
`colors_max`, which stay at zero. Over the SDK that shows as
`colors=0..0` for both modes (`argb devices show 3`). The server validates
every mode update in `RGBController::SetModeValuesFromMode` and refuses one
whose color list is longer than `colors_max`, so a Static update carrying
the color is dropped, and one carrying no color is accepted and rendered by
the driver as `colors[0]` of an empty list: black. The same applies to
Breathing. OpenRGB's own window never sees this because it edits the
driver's one-entry list in process.

Nothing on the client side can get around it: the check compares the
server's own `colors_max`. The fix belongs upstream (set `colors_min` and
`colors_max` to 1 on both modes in `RGBController_LogitechG203L.cpp`; the
`SetMode` packet already carries the color). Until an OpenRGB release
carries it:

- **Static while awake**: Direct mode, which works and is what the service
  has used since Phase 1. Brightness is applied by scaling the color, as
  the mode has no brightness control.
- **Pulse while awake**: the engine's software pulse over Direct, in sync
  with the rest.
- **Sleep**: the mouse loses USB power when the machine sleeps, so nothing
  it shows matters. The adapter no longer selects a Breathing mode that
  cannot carry a color, so the sleep lighting written before suspend is
  the pulse color shown statically over Direct rather than a black
  breathing.
- **Boot**: OpenRGB cannot write the onboard profile. The mouse shows its
  onboard lighting from power-on until the service applies the active
  state, a few seconds after OpenRGB finishes detecting. Setting that
  onboard profile once in G HUB to the active color (static, full
  brightness) makes POST and boot match the configuration; the service
  then takes over without a visible change.

Known upstream behavior to watch: OpenRGB issue 877 reports a G203 that
stops accepting updates after some time on Windows until it is
replugged. Not reproduced here so far; the resume re-detection would cover
a stale handle the same way it does for the motherboard.

## Cloud Nine C959 ErgoTKL (keyboard)

| Fact | Value |
|---|---|
| USB id | `0C45:8008`, revision `0117` (firmware 1.17.00), bus-reported name "USB-Keyboard" |
| Chip family | Sonix (the vendor id); the app's `device.xml` names `0C45:8006`, the same family as the Svive Tritium and AUKEY KM-G12 |
| Interfaces | 0: keyboard; 1: consumer control, system control, a second keyboard, a mouse, a vendor-defined collection (usage page `0xFFFF`, usage 1), a multi-axis controller |
| Control path | 64-byte HID feature reports (report id 0) on the keyboard collection of interface 0, the only collection that acknowledges a command |
| Vendor app | "The Cloud Nine Keyboard Application" 2.0.1.15, `DeviceDriver.exe`, a generic Sonix OEM configurator (MUI skins, SQLite, `KeyboardLayout.xml` for the C959); Windows only |
| OpenRGB | not supported. OpenRGB's Sonix keyboards (EVision family) use usage page `0xFF1C`; nothing in OpenRGB matches `0C45:8008` |
| Windows Dynamic Lighting | no LampArray collection (the G203 has one) |
| Backend | `argb-backend-sonix`, `backend = "sonix"`; `argb devices scan` adds the keyboard as `cloud-nine-c959-ergotkl` |

### Protocol

The same `DeviceDriver.exe` family configures the Ajazz AK820 Pro
(`0C45:8009`/`8006`) and the AULA F108 Pro, whose protocol two projects
reverse-engineered: `SpawW/ajazzctl` (`PROTOCOL.md`) and
`donglover69/sonix-keyboard-studio`. Their framing holds for the C959;
the lighting payload does not, which a USB capture of the Cloud Nine app
settled.

Framing, confirmed on the C959:

| Element | Value |
|---|---|
| Report | feature report, id 0, 64 payload bytes; `SET_REPORT`/`GET_REPORT` with `wValue 0x0300`, `wIndex 0` |
| Command | payload `04 CMD`, argument at payload byte 8 |
| Acknowledgement | the next `GET_REPORT` echoes the command with payload byte 3 = `01` (`FF` refuses it) |
| Pacing | 35 ms between reports; the application retries commands that are not acknowledged |
| Session | `04 18` begin, `04 13 … COUNT` select the lighting section with its page count at byte 8, COUNT data pages, `04 02` commit (its echo carries two checksum-like bytes), `04 F0` apply |
| Other sections seen | `11` and `27` (key remap, 10 pages), `15` (opened with `04 19`, 8 pages) and `25` (1 page) during a profile apply, `17 05` (1 page) system settings; `F5` (read back the live table on the siblings) is refused |

The lighting section (`13`) on the siblings is one 16-byte effect record.
The C959 declares it with 18 pages, and the application rewrites all 1152
bytes on every change, including a color change:

| Offset | Content |
|---|---|
| 0 | 20 preset records of 16 bytes: one per effect 1 to 19, then the per-key mode (`80`) |
| 320 | 192 zero bytes |
| 512 | 144 per-key entries, each `80 R G B` |
| 1088 | the active record: a copy of the preset the keyboard should run |
| 1104 | zero bytes to the end of page 18 |

A record is `MODE R G B 00 00 00 00 RAINBOW BRIGHTNESS SPEED DIRECTION 00
00 AA 55`. The application's presets are white, cyan, red, yellow and blue
for effects 1 to 5, `A0 20 F0` with the rainbow flag from effect 7 on,
brightness and speed 6 everywhere, and brightness 5 on the per-key
preset. Choosing an effect copies its preset into the active record;
changing its color, brightness or speed edits both. Brightness went from 6
(the application's "High") to 1 ("Dim") and speed between 5 and 9. Effect
ids follow the application's visible effect list, not its string table
(whose entries 200 to 219 are in another order): 1 is "Static color", 2
a rainbow effect, 3 "Falling" and 5 "Breath", checked on the keyboard;
the application's list gives the rest.

A block with an all-zero active record turns the LEDs off, which is what
every partial write and the application without a loaded profile produced:
the application writes all zeros when its "Profile" header is empty, and
the first captures were all zeros for that reason. The firmware stores the
block in flash and keeps it across a replug, so the backend treats the
keyboard as persistent, writes a block only when it differs from the last
one written, and offers no direct mode: a session takes close to a second
and would wear the flash at the pulse tick rate. The sleep pulse uses the
breathing effect with a speed derived from the period (logarithmic between
the period bounds; the keyboard's own timing is not measured yet).

### Capture notes

- USBPcap on the root hub carrying the keyboard, all devices, 512-byte
  snapshot. The two optical drives on the same hub flood the file with
  bulk polling (the first capture reached 5.8 GB in six minutes); a 45
  second window per experiment keeps it readable.
- tshark fields are empty for the payload of control transfers; the
  scratch parser read the `-x` hex dump against the USBPcap header
  (function at bytes 14 and 15, info at 16, device address at 19 and 20,
  endpoint at 21, transfer type at 22, data length at 23 to 26, setup
  stage at 27, then the 8 setup bytes and the data).
- The keyboard's USB address changed between captures (11, then 14); read
  it from the `GET_DESCRIPTOR` responses (`usb.idVendor`) rather than
  assuming it.
- `argb-sonix-probe` replays sessions from the console (`light`, `fill`,
  `off`, raw `session` and `raw`) for experiments the backend does not
  cover.

### Recovery

Any write with an empty active record leaves the keyboard dark; it is not
broken. Pressing the keyboard's own effect key restores a firmware effect,
and `argb apply active` or `argb-sonix-probe light` restores the
configured lighting. The vendor application needs a profile loaded (the
"P" page) before its Lighting page does anything.

## Upstream report for the G203

Title: "Logitech G203L: Static and Breathing modes declare colors_max 0,
so SDK clients cannot set their color". Body: in
`RGBController_LogitechG203L.cpp` the `Static` and `Breathing` modes call
`colors.resize(1)` without setting `colors_min`/`colors_max`, which stay at
0. `SetModeValuesFromMode` rejects any SDK update whose `colors.size()`
exceeds `colors_max`, and an update with no colors reads `colors[0]` of an
empty vector in `DeviceUpdateMode`, so SDK clients can only turn the light
off in these modes. Fix: add `Static.colors_min = 1; Static.colors_max = 1;`
and the same for `Breathing`, next to the `colors.resize(1)` calls.

## Status

- G203: understood and worked around; upstream fix to file with the
  OpenRGB project.
- C959: protocol captured and documented; `argb-backend-sonix` drives the
  static color, brightness and the breathing effect from the service and
  the CLI. To verify on the machine: what the keyboard shows while the
  host sleeps (whether the port keeps it powered) and during POST, and the
  breathing speed scale.
