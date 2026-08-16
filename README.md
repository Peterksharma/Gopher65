<div align="center">

# Gopher65

**An N64 emulator for playing together — a friendly fork of [gopher64](https://github.com/gopher64/gopher64).**

[Download](../../releases) · [Features](#what-gopher65-adds) · [Netplay guide](#netplay-quick-start) · [Building](#building)

</div>

---

## Why this fork exists

Gopher65 is built on **[gopher64](https://github.com/gopher64/gopher64)**, an excellent
cross-platform N64 emulator by **loganmc10**. Enormous credit goes to loganmc10 and the
gopher64 contributors — Gopher65 is their emulator with a few extra features layered on top.

Those features were developed to improve local + online multiplayer, but they fall outside
what the upstream project accepts as contributions, so rather than let them go to waste they
are maintained here as an independent fork. **Gopher65 is not affiliated with or endorsed by
the gopher64 project** — please don't file Gopher65 issues on their tracker or ask loganmc10
for support with this build. Use [this repo's issues](../../issues) instead.

## What Gopher65 adds

Everything else is stock gopher64. On top of it, Gopher65 adds:

- 🎮 **Nintendo Switch 2 Pro Controller support on macOS.**
  Upstream SDL's Switch 2 driver can't claim the HID interface the macOS kernel holds, so the
  controller doesn't work there. Gopher65 builds SDL from a small macOS-only patch that talks
  to the controller over its vendor USB interface. Compiled statically into the app — nothing
  extra to install.

- 🕹️ **Multiple local players in one netplay session.**
  A single machine can drive more than one local player online — couch co-op *and* networked
  players in the same game (e.g. two players on one Mac + a third across the internet).

- 📡 **Host your own netplay session — no third-party server.**
  `--netplay-host` runs the matchmaking rendezvous in-process, so one player hosts directly and
  everyone connects to them. No cloud service, no accounts. Gameplay is direct peer-to-peer.

- 📊 **In-game network overlay (press F1).**
  FPS plus ping, up/down bandwidth, and rollback lead/lag — so you can tune input delay live.

> **Netplay compatibility:** Gopher65 uses its own signaling and does **not** connect to the
> official gopher64 lobby. Everyone in a session must run **Gopher65** (matching versions).

## Download

Grab the latest build from **[Releases](../../releases)**.

| Platform | File | First-run note |
|---|---|---|
| **macOS** (Apple Silicon) | `gopher65-macos-aarch64.zip` | Not notarized — **right-click the app → Open** the first time (or run `xattr -cr Gopher65.app`). |
| **Windows** (x64 / ARM64) | `gopher65-windows-*.exe` | Unsigned — SmartScreen may warn: **More info → Run anyway**. |

> Windows Switch 2 controller support is **untested** — the controller fix is macOS-specific;
> on Windows SDL may already handle the pad, but no one has verified it. Reports welcome.

## Netplay quick start

Player numbers are 0-based (P1 = `0`, P2 = `1`, …). `--netplay-number-of-players` is the
**total** across all machines. All players must load the **exact same ROM** (byte-identical).

**Host** (also plays; brokers the handshake in-process):

```
gopher65 --netplay-host --netplay-room gauntlet \
         --netplay-player-number 0 \
         --netplay-number-of-players 3 \
         --netplay-input-delay 2 \
         "/path/to/rom.z64"
```

Prints `Embedded signaling server listening on 0.0.0.0:3536`.

**Joiners** point at the host's IP and the same room:

```
gopher65 --netplay-server-addr ws://HOST_IP:3536/gauntlet \
         --netplay-player-number 2 \
         --netplay-number-of-players 3 \
         --netplay-input-delay 2 \
         "/path/to/rom.z64"
```

**Couch co-op:** to drive two local players on one machine, pass `--netplay-player-number`
twice (`--netplay-player-number 0 --netplay-player-number 1`) and enable both ports in
Controller Configuration, one physical controller each.

Notes:
- On a **LAN** this needs no setup. Over the **internet**, the host forwards the signaling port
  (default `3536`, override with `--netplay-host-port`); gameplay is direct P2P (STUN-assisted),
  so it uses little bandwidth. Very strict/CGNAT networks may need a TURN relay.
- Press **F1** in-game for the FPS / network overlay; lower input delay while lead/lag stays
  near zero.

## Building

1. Linux only: [install the SDL3 build dependencies](https://wiki.libsdl.org/SDL3/README-linux#build-dependencies).
2. [Install Rust](https://www.rust-lang.org/tools/install).
3. `git clone --recursive https://github.com/Peterksharma/Gopher65.git`
4. `cd Gopher65`
5. `cargo build --release`
6. `./target/release/gopher64 /path/to/rom.z64`

The patched SDL is vendored in `third_party/sdl3-src-patched/` and wired via `[patch.crates-io]`,
so it compiles statically into the binary — no extra steps.

## Controls & general use

Gopher65 inherits gopher64's controls and features. See the upstream
[wiki](https://github.com/gopher64/gopher64/wiki) for keyboard/gamepad defaults, homebrew,
savestates, cheats, and RetroAchievements.

## License

Gopher65, like gopher64, is licensed under the **GPLv3** (see [LICENSE](LICENSE)). Portions of
gopher64 are adapted from mupen64plus and/or ares
([mupen64plus license](https://github.com/mupen64plus/mupen64plus-core/blob/master/LICENSES),
[ares license](https://github.com/ares-emulator/ares/blob/master/LICENSE)).
`third_party/sdl3-src-patched/` is SDL (zlib license) with macOS-only patches; its license is
retained in that directory.

## Privacy

During netplay, the host's signaling server sees connecting players' IP addresses as part of
establishing the peer-to-peer connection (the same information any direct connection reveals).
This project collects and stores nothing. If you enable RetroAchievements, some data is sent to
their systems — see their [terms](https://retroachievements.org/terms).
