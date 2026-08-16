# Gopher65

**Gopher65 is a fork of [gopher64](https://github.com/gopher64/gopher64)** (a
cross-platform N64 emulator by loganmc10) that adds three things on top of it:

1. **Nintendo Switch 2 Pro Controller support on macOS.** Upstream SDL's Switch 2
   driver is libusb-only and cannot claim the HID interface the macOS kernel
   holds, so the controller does not work there. Gopher65 builds SDL from a
   minimally-patched copy that opens its own libusb handle on the controller's
   vendor bulk interface. The patch is macOS-only and inert on every other
   platform.
2. **Multiple local players in netplay.** One machine can drive more than one
   local player in an online session (couch co-op alongside networked players).
3. **Embedded host signaling + an on-screen net overlay.** A player can host the
   netplay rendezvous in-process (`--netplay-host`) — no third-party or cloud
   server — and an F1 overlay shows FPS plus ping / bandwidth / rollback lead &
   lag for tuning.

Everything else is stock gopher64.

## Relationship to upstream

This is an **independent fork and is not affiliated with or endorsed by the
gopher64 project.** Please **do not** file Gopher65 issues on the upstream
tracker or ask loganmc10 for support with this build — use this repository's
issue tracker instead. Enormous credit and thanks to loganmc10 and the gopher64
contributors for the emulator this is built on.

> **Netplay compatibility:** Gopher65 uses its own signaling and does not connect
> to the official gopher64 lobby. Everyone in a session must run **Gopher65**
> (matching versions). It will not matchmake with players on stock gopher64.

## Downloads

Grab the latest build from this repository's [Releases](../../releases).

- **Windows** (x86_64 / arm64): standalone `.exe`. Unsigned — SmartScreen will
  warn on first run; choose *More info → Run anyway*.
- **macOS** (Apple Silicon): `.app` in a `.zip`. Not notarized — on first launch
  **right-click the app → Open** (or run `xattr -cr Gopher65.app` once).

## Netplay usage

Player numbers are 0-based (P1 = 0, P2 = 1, …). `--netplay-number-of-players` is
the **total** across all machines.

**Host** (also plays; brokers the handshake in-process — no external server):

```
gopher65 --netplay-host \
         --netplay-room gauntlet \
         --netplay-player-number 0 \
         --netplay-number-of-players 3 \
         --netplay-input-delay 2 \
         "/path/to/rom.z64"
```

**Joiners** point at the host's LAN/public IP and the same room:

```
gopher65 --netplay-server-addr ws://HOST_IP:3536/gauntlet \
         --netplay-player-number 2 \
         --netplay-number-of-players 3 \
         --netplay-input-delay 2 \
         "/path/to/rom.z64"
```

**Couch co-op:** to drive two local players on one machine, pass
`--netplay-player-number` twice (e.g. `--netplay-player-number 0
--netplay-player-number 1`) and enable both ports in Controller Configuration,
one physical controller each.

Notes:
- Default host port is `3536` (override with `--netplay-host-port`).
- On a **LAN** this needs no setup. Over the **internet**, the host must
  forward the signaling port; gameplay itself is direct P2P (STUN-assisted),
  so it uses little bandwidth. Very strict/CGNAT setups may need a TURN relay.
- All players must load the **exact same ROM** file (byte-identical).
- Press **F1** in-game for the FPS / netplay overlay; lower input delay while
  watching lead/lag stay near zero.

## Building

1. Linux only: [install the SDL3 build dependencies](https://wiki.libsdl.org/SDL3/README-linux#build-dependencies).
2. [Install Rust](https://www.rust-lang.org/tools/install).
3. `git clone --recursive https://github.com/Peterksharma/Gopher65.git`
4. `cd Gopher65`
5. `cargo build --release`
6. `./target/release/gopher64 /path/to/rom.z64`

The patched SDL is vendored in `third_party/sdl3-src-patched/` and wired in via
`[patch.crates-io]`, so no extra steps are needed — it compiles statically into
the binary.

## Controls & general use

Gopher65 inherits gopher64's controls and features. See the upstream
[wiki](https://github.com/gopher64/gopher64/wiki) for keyboard/gamepad defaults,
homebrew, savestates, cheats, and RetroAchievements.

## License

Gopher65, like gopher64, is licensed under the **GPLv3** (see [LICENSE](LICENSE)).
Many portions of gopher64 have been adapted from mupen64plus and/or ares; the
mupen64plus license is [here](https://github.com/mupen64plus/mupen64plus-core/blob/master/LICENSES)
and the ares license is [here](https://github.com/ares-emulator/ares/blob/master/LICENSE).

`third_party/sdl3-src-patched/` is SDL (zlib license) as distributed in the
`sdl3-src` crate, with the macOS-only patches described above; its license is
retained in that directory.

## Privacy

During netplay, the host's signaling server sees the connecting players' IP
addresses as part of establishing the peer-to-peer connection (the same
information any direct connection reveals). No data is collected or stored by
this project. If you enable RetroAchievements, some data is sent to their
systems — see their [terms](https://retroachievements.org/terms).
