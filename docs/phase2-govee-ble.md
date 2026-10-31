# Phase 2: Govee lights over Bluetooth LE

Working notes for the Govee backend: what the community knows about the
protocol, what the spike verified on the two lights, and the backend design
that followed. The backend lives in `crates/argb-backend-govee`; the spike
that preceded it (`crates/argb-ble-probe`) was folded into that crate and
remains in the history of the branch that added it.

## Targets

| Device | Model | Advertised name | Manufacturer data header | Channel |
|---|---|---|---|---|
| Glide Hexa light panels (10 panels) | H6061 | `Govee_H6061_xxxx` | company id `0x8803`, pact (1,3) | plaintext |
| Lyra floor lamp (7 segments) | H6076 | `Govee_H6076_xxxx` | company id `0x8843`, pact (2,1) | encrypted |

Both advertise continuously while powered, whether on or off, and accept one
central at a time: the Govee phone app must not be connected.

## Protocol facts

All from community reverse engineering (sources at the end); every item marked
"verified" was confirmed on the two lights above on 2026-10-31.

### GATT

- Service `00010203-0405-0607-0809-0a0b0c0d1910` (verified).
- Commands are written to `...2b11`, write without response (verified).
- Replies and status arrive as notifications on `...2b10`; subscribe before
  writing (verified: nothing arrives on `2b11`).
- Some models expose `...2b12` with an "encrypt version" byte; neither target
  does, so the advertisement decides the channel.

### Frames

Every frame is 20 bytes: byte 0 the frame type, byte 1 the operation, the
payload zero-padded, byte 19 the XOR of bytes 0-18.

| Frame | Bytes | Reply |
|---|---|---|
| Keep-alive / power read | `aa 01 00.. ab` | `aa 01 <on> .. ` (verified on both) |
| Power | `33 01 <01 or 00>` | `33 01 00 ..` (verified) |
| Brightness, percent | `33 04 <0-100>` | |
| Color, H6061 | `33 05 15 01 R G B 00 00 <mask hi> <mask lo>` with mask `ff 03` for all ten panels | `33 05 00 ..` (verified) |
| Color, H6076 | `33 05 15 01 R G B 00 00 00 00 00 <mask lo> <mask hi>` with mask `7f 00` for all seven segments | `33 05 00 ..` (verified) |
| Manual color, older family | `33 05 02 R G B` | not applicable to the targets |

Neither target has a whole-device color opcode; the full-mask segment write
is what the vendor app sends. Do not send the brightness frame on every
reconnect: it flashes the panels.

### Encrypted channel (H6076)

Bit `0x40` of the company id's low byte announces it. Every frame on the
channel, both directions, is AES-128 ECB without padding on the first 16
bytes and RC4 with the same key on the last 4. The session opens with a
handshake under the published pre-shared key `MakingLifeSmarte`:

1. Host writes `e7 01 <17 fill bytes> <xor>`, encrypted with the pre-shared
   key. The light answers `e7 01 <16-byte session key> ..`, encrypted the
   same way (verified: the reply came 30 ms later).
2. Host writes `e7 02 <fill> <xor>`, encrypted with the pre-shared key; the
   light echoes an `e7 02` acknowledgement (verified).
3. Everything after is encrypted with the session key (verified: the three
   acknowledgements above decrypt correctly).

The light retransmits its handshake replies about a second later; the probe
ignores frames whose wire bytes it has already seen. No account secret
(`33 b2`) is involved for either target.

### Keep-alive and idle

The vendor app sends the power read every 2 s while connected, and the
firmware is reported to drop a link idle for about 10 s. The community
implementations connect, write, and disconnect after 8 s idle (30 s after a
command). Whether a light keeps its color across a disconnect is not stated
anywhere; both lights kept the color after the probe disconnected, which is
the behaviour the backend needs.

### Built-in effects

Neither target offers a documented breathing effect. The H6061 has no known
scene codes at all; the H6076 has a 110-entry scene catalogue whose closest
entry ("Heartbeat") needs a multi-frame parameter upload. The backend
therefore reports no hardware pulse: in the sleep state these lights show
the pulse color statically, which the engine already does for such devices.

## Spike results (2026-10-31)

Console session, Windows 11, MediaTek Bluetooth adapter, `btleplug` 0.13.5 on
its WinRT backend:

- `argb-ble-probe scan` lists both lights with their names and manufacturer
  data in a 10 s scan.
- `argb-ble-probe connect --name H6061 --layout hexa --color 40e0d0`: the
  Hexa acknowledges power and color.
- `argb-ble-probe connect --name H6076 --layout lamp --color 40e0d0`: the
  lamp completes the handshake and acknowledges power and color.

Bluetooth LE from session 0 as `LocalService`: verified the same day. The
probe installed as a service scanned 15 peripherals, connected to the Hexa,
completed the exchange and painted it, with nothing published beforehand to
say the WinRT Bluetooth APIs would work there. The backend can therefore live
inside the service.

## Backend (crate `argb-backend-govee`)

- `LightingDevice` for each configured Govee light: `backend = "govee_ble"`
  in the configuration, with `match_name` matched against the advertised
  name as for OpenRGB. `argb devices scan` hears the lights and adds the
  supported models.
- Modules: `protocol` (frames, layouts, masks), `crypto` (AES-128 ECB plus
  RC4 under the pre-shared key, then the session key), `model` (H6061 and
  H6076 recognized from the advertised name), `transport` (one Tokio runtime
  shared by all links, a `BleLink` per light that scans, connects, runs the
  handshake when the advertisement asks for it, writes, drains the
  notifications, and disconnects), `device` (the adapter), `discovery`
  (devices from the profile, and the scan).
- A static apply is one short connection: power on, color, brightness when
  it changed, disconnect. Direct mode keeps the link open and writes at most
  one color frame per 100 ms while the engine ticks at 30 Hz.
- Capabilities: hardware brightness (the `33 04` frame), no hardware pulse
  (neither light has a breathing effect, so the sleep pulse shows its color
  statically), persistent memory (the lights keep the last state on their
  own once the link is closed, which the sleep lighting relies on; the
  deferred cold-boot probe will confirm the power-cycle case).
- Failure handling lives in the service: a Govee error marks that device as
  failed in the engine, and the service retries the failed devices 10
  seconds later; OpenRGB keeps running meanwhile.
- The layout (segment mask position) comes from the model code in the
  advertised name.

Verified on 2026-10-31 with both lights: `argb devices scan` adds them,
`argb apply active` reaches all seven configured devices with no failure,
and the service in console mode builds the engine with both lights and
connects to each on its first apply.

## Sources

- mpalczew, *govee-ble-segments* (MIT): H6061 frames and panel masks.
  https://github.com/mpalczew/govee-ble-segments
- Brady Woods, *govee-ble-local* 1.10.0 on PyPI (MIT): advertisement
  parsing, encrypted channel, handshake, keep-alive timings, device table.
  https://pypi.org/project/govee-ble-local/
- teh-hippo, *ha-govee-led-ble* (MIT): H6076 frame layout, scene catalogue,
  test vectors. https://github.com/teh-hippo/ha-govee-led-ble
- egold555, *Govee-Reverse-Engineering* (no license; facts only):
  frame format and H6061 notes.
  https://github.com/egold555/Govee-Reverse-Engineering
- deviceplug, *btleplug* (MIT/Apache-2.0/BSD-3): the Rust BLE library.
  https://github.com/deviceplug/btleplug
