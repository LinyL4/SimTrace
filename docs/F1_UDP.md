# EA SPORTS F1 UDP setup

SimTrace uses one UDP listener and automatically selects a decoder from the packet format identifier. F1 25 UDP Format 2025 and the F1 25: 2026 Season Pack UDP Format 2026 are separate decoders with their own packet sizes and vehicle-array layouts. Native 2026 Season Pack output is supported; the game does not need to use its legacy F1 25 format.

EA's 2026 structure document labels the 2026 header game-year value as `26`. A live F1 25: 2026 Season Pack build was observed sending packet format `2026` with game year `25` and game version `1.26`, while its packet IDs and lengths matched the documented 2026 layouts. The 2026 decoder therefore accepts game year `25` or `26` only after packet format `2026` selects that decoder. This compatibility allowance does not alter any packet layout or offset.

The implementation follows EA's current official [F1 25 and 2026 Season Pack UDP specification page](https://forums.ea.com/blog/f1-games-game-info-hub-en/ea-sports%E2%84%A2-f1%C2%AE25-2026-season-pack-udp-specification/12187347), [F1 25 v3 specification](https://forums.ea.com/t5/s/tghpe58374/attachments/tghpe58374/f1-games-game-info-hub-en/61/4/Data%20Output%20from%20F1%2025%20v3.pdf), [F1 25 structures](https://forums.ea.com/t5/s/tghpe58374/attachments/tghpe58374/f1-games-game-info-hub-en/61/5/F1%2025%20Telemetry%20Output%20Structures.txt), and [2026 Season Pack structures](https://forums.ea.com/t5/s/tghpe58374/attachments/tghpe58374/f1-games-game-info-hub-en/61/8/2026%20Season%20Pack%20Telemetry%20Output%20Structures%20(1).txt).

## Direct game output

1. In SimTrace, open settings and select **EA SPORTS F1 (UDP 2025/2026)**.
2. Set **Listen address** to `127.0.0.1` when the game and SimTrace run on this PC. Use `0.0.0.0` when SimTrace must accept packets on any network interface.
3. Choose an unused **UDP port**. `20777` is EA's usual default, but SimTrace does not require it.
4. Save the SimTrace settings.
5. In F1 25 telemetry settings, turn UDP telemetry on, set the UDP IP address to `127.0.0.1`, and enter the same port.
6. For the first 2026 Season Pack test, select its native UDP Format 2026. A 60 Hz send rate is suitable.

SimTrace accepts unicast or broadcast datagrams. The sender address is not used to distinguish direct game packets from forwarded packets.

## Sharing telemetry with SimHub or Project Aeternum

Two applications on the same Windows PC should not both depend on owning the same unicast UDP endpoint. Give one application the game-facing port and forward a copy to a different SimTrace port.

Example:

1. Let SimHub or Project Aeternum receive the game's UDP stream on `20777`.
2. Enable that application's UDP forwarding to `127.0.0.1:20778`.
3. Set SimTrace to listen on `127.0.0.1` and port `20778`, then save.
4. Keep the forwarded packet payload unchanged. SimTrace handles it exactly like a direct game datagram.

SimHub documents its forwarding setup in [Sharing UDP data with other applications](https://github.com/SHWotever/SimHub/wiki/Sharing-UDP-data-with-other-applications/3d41406eaa9474eed8e10dec14ebd3d66a9d2fee). SimTrace does not forward packets in this phase.

## First live test

With the 2026 Season Pack sending native Format 2026, verify that:

- speed, RPM, gear, throttle, brake, clutch, and normalized steering respond for the player's car;
- trace history continues moving without mouse or window events;
- the selected 10-second or 30-second trace window remains bounded;
- opening and closing the Phase Plot does not interrupt collection;
- restarting SimTrace retains the listen address, port, trace window, and display refresh rate.

### Live diagnostic log

On Windows, receive and decoder diagnostics are written to `%APPDATA%\simtrace\simtrace.log.YYYY-MM-DD`. The first few distinct packet headers include the sender, datagram length, format/year, game major/minor version, packet version, packet ID/type, and player-car index. A summary is emitted every five seconds while packets arrive, with counters for protocol detection, validation, telemetry/status decoding, normalized frames, and categorized rejections. Each rejection category logs at most its first three examples.

For a failed live test, preserve the lines containing `F1 UDP datagram header observed`, `F1 UDP datagram rejected`, and `F1 UDP diagnostic summary`. These values should be compared with the current EA specification before any decoder layout is changed.

F1 car status packets expose the configured ABS and traction-control assistance modes. They do not identify moment-by-moment intervention, so SimTrace does not invent ABS-active or TC-active samples. The initial decoder also leaves physical wheel angle and wheel slip unsupported until a later packet integration provides those channels correctly.
