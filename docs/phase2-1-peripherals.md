# Phase 2.1: USB peripherals

Working notes for the two USB peripherals: what each one exposes, how far it
can be driven today, and what remains.

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
  cannot carry a color (this branch), so the sleep lighting written before
  suspend is the pulse color shown statically over Direct rather than a
  black breathing.
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
| Vendor app | "The Cloud Nine Keyboard Application" 2.0.1.15, `DeviceDriver.exe`, a generic Sonix OEM configurator (MUI skins, SQLite, `KeyboardLayout.xml` for the C959); Windows only |
| OpenRGB | not supported. OpenRGB's Sonix keyboards (EVision family) use usage page `0xFF1C`; nothing in OpenRGB matches `0C45:8008` or usage page `0xFFFF` |
| Windows Dynamic Lighting | no LampArray collection (the G203 has one) |

The lighting is set from the vendor app or from the keyboard's own keys
(Fn plus the wheel for brightness). The control path is therefore the
vendor-defined collection on interface 1, with a protocol nobody has
published. Establishing it means capturing what the app sends.

### Capture procedure (next step)

USBPcap and Wireshark are installed. From an elevated prompt, with the
Cloud Nine app closed:

```
& "C:\Program Files\USBPcap\USBPcapCMD.exe"
```

Pick the root hub that lists `USB-Keyboard` (vendor `0C45`), accept the
default capture file, and leave it running. Open the Cloud Nine app and
perform a fixed script, waiting five seconds between steps so the capture
can be read against the clock:

1. Set the whole keyboard to static red.
2. Static green.
3. Static blue.
4. Brightness to its lowest value, then back to full.
5. Any built-in effect (for its opcode), then back to static.
6. Close the app.

Stop the capture with Ctrl+C and open the file in Wireshark with the
filter `usb.transfer_type == 0x02 || usbhid` restricted to the keyboard's
device address. The frames of interest are SET_REPORT control transfers
or interrupt OUT writes on interface 1 whose payload changes with the
color chosen; their report id, length, and the position of the red, green
and blue bytes give the frame layout. A per-key write usually shows as a
burst of frames with a running offset.

What the capture decides:

- If the app sends plain HID output or feature reports, a
  `argb-backend-sonix` crate can replay them through `hidapi`, and the
  keyboard follows the configuration like the others. Persistence across
  a reboot is then tested the usual way.
- If the app only writes a configuration blob that the firmware interprets
  (common with these OEM apps), the backend can still write a static color
  into that blob, but live effects would stay the firmware's.
- If the traffic is a firmware-style upload, the keyboard is documented as
  unsupported.

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

- G203: understood and worked around (this branch); upstream fix to file
  with the OpenRGB project.
- C959: identified; capture pending.
