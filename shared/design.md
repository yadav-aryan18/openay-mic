# OpenAY Mic — Product Design Brief v1 ("Studio rack at night")

Single source of truth for BOTH native UIs (Android Compose + desktop Iced).
The two apps must read as one product. Every color/type decision below is
binding; free axes are marked "free".

## Subject

OpenAY Mic turns a phone into a studio-grade microphone for the desktop.
Audience: streamers, podcasters, tinkerers. The interface's single job: make
the link state legible at a glance and starting/stopping effortless.

The product's world is **broadcast/studio hardware**: consoles, patch bays,
illuminated VU meters, rack units, engraved labels, tally lamps. The UI speaks
that vernacular — it is a rack unit rendered in software, not a web dashboard.

## Signature element — "The Chain"

A horizontal signal-path strip is the hero of both apps:

    MIC ──── LINK ──── CONSOLE

Three stages joined by cable segments. Each stage is a real instrument bound
to real pipeline state (structure encodes information):

- MIC — level ring driven by live capture level (`level_peak`).
- LINK — packet rate + loss readout (mono digits, e.g. `480/s · 0 lost`).
- CONSOLE — jitter-fill gauge (`10.0 ms`).

Cable segments pulse while audio flows; the strip is dim/"cold" when idle and
amber-lit/"hot" when streaming. On Android the phone IS the MIC stage (its
ring shows what the mic hears even before connecting).

## Palette (binding)

| Token      | Hex     | Role                                            |
|------------|---------|-------------------------------------------------|
| ink        | #14120F | app background — warm near-black, brown-tinted  |
| panel      | #1E1B16 | raised surfaces/cards                           |
| line       | #34302A | hairlines, segment tracks                       |
| cream      | #EFE6D4 | primary text — illuminated VU-face cream        |
| amber      | #FFB454 | active/live accents, lit meters                 |
| tally      | #E5484D | ONLY the ON AIR lamp + clip zone                |
| dim        | #8D8477 | secondary text, disabled                        |

Rules: warm grays everywhere (never blue-gray). tally red appears in exactly
two places: the live lamp and the top VU clip segments. No gradients except
subtle vertical shading on panels to suggest machined metal (<= 6% lightness).

## Typography (binding)

- Display/labels: **Chakra Petch** (SemiBold for headers/lamp, Medium for
  labels) — squared, technical, hardware-silkscreen character. ALL-CAPS with
  wide tracking (+8%) for stage labels.
- Data/numbers: **IBM Plex Mono** (Regular/Medium) — every number on screen:
  ports, rates, ms values, IPs.
- Body (Android only where Chakra is too wide): Roboto default is acceptable,
  but labels stay Chakra Petch.

Font files ship in `shared/fonts/` (OFL); both apps bundle them.

## Components

- **ON AIR lamp/toggle**: circular button, 96 dp/px class. States:
  - COLD: engraved ring (line), label "STANDBY" (dim)
  - ARMED/HOT: amber glow ring, label "ON AIR" (cream), tally-red center dot
  - Pressing animates a short ring pulse (150–250 ms), no bounce/spring toys.
- **VU ladder** (desktop primary meter): 24 horizontal segments, mapped from
  peak level with ~1 dB/segment top-end weighting; bottom 18 cream, next 3
  amber, top 3 tally red (clip). Unlit = line color. Decay ~12 dB/s ballistics.
- **Stage cards**: panel background, 2 px radius max (machined, not bubbly),
  hairline border, engraved caps label (Chakra, dim), value in Plex Mono cream.
- **Copy voice**: console terse. "ON AIR", "STANDBY", "LINK 480/s", "0 LOST",
  "JITTER 10.0 ms", "CLIP". Sentence case elsewhere; verbs literal
  ("Save settings" not "Submit").

## Motion

One orchestrated moment: flipping the lamp runs a chain power-on sequence —
stages light left-to-right over ~400 ms with the cables pulsing once. Ambient
motion elsewhere limited to: VU decay ballistics, cable pulse while hot
(~1 Hz), level ring smoothing (~100 ms ease). Respect reduced-motion by
dropping cable pulses and the power-on stagger.

## Layout sketches

Desktop window (~460×600, resizable min 420×520):

    ┌──────────────────────────────────┐
    │ ● OPENAY MIC            [≡]      │ header: lamp-dot + wordmark + menu
    │                                  │
    │ ╭─ MIC ─╮────╭─ LINK ─╮─╭CONSOLE╮│  The Chain (hero card)
    │ │ (oo)  │    │ 480/s  │ │ 10.0ms││
    │ ╰───────╯    ╰────────╯ ╰───────╯│
    │ ▮▮▮▮▮▮▮▮▮▮▮▮▮▮▯▯▯▯▯▯▯▯  VU PEAK  │  segmented ladder
    │                                  │
    │        ⦿  ON AIR                 │  big toggle (centered)
    │                                  │
    │ UDP · 0.0.0.0:41700 · AUTO       │  status line (mono, dim)
    └──────────────────────────────────┘

Settings = slide-over from right (same window): port field, bind-address
dropdown (local interfaces), codec AUTO/PCM/OPUS chips, jitter target slider
5–20 ms, autostart switch, start-minimized switch, "Save settings".

Android: single scrollable screen, dark only.
    [status bar blends into ink]
    OPENAY MIC                    (wordmark, Chakra SemiBold caps)
    ╭ The Chain hero card ─────────────╮
    │ MIC(ring) ── LINK ── CONSOLE     │
    ╰──────────────────────────────────╯
         ⦿ ON AIR   (96dp circular toggle)
    TRANSPORT   [ WI-FI | USB | BT-soon ]
    CODEC       [ RAW PCM | OPUS ]  FRAME [5|10 ms]
    ╭ NETWORK ─────────────────────────╮
    │ HOST 10.0.2.2  PORT 41700        │
    │ SENT 1,240 · 0 LOST · UP 00:12   │
    ╰──────────────────────────────────╯

## Anti-goals

No purple/blue gradient heroes, no glassmorphism, no rounded-3xl bubbles, no
emoji icons, no confetti/springs, no dashboard grid of identical stat cards
(the Chain's three stages are instruments with distinct content, not tiles).
