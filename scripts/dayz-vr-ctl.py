#!/usr/bin/env python3
"""Talk to the DayZ VR debug plugin while DayZ is running.

The plugin (dayz_openxr_debug.dll, enabled by [debug] in dayz_openxr.ini) listens on
127.0.0.1:<port> inside the Proton/Wine process, which is the same loopback as the
host, so this script needs no Wine. Examples:

    dayz-vr-ctl.py get                      # full state as JSON
    dayz-vr-ctl.py get hmd_yaw host_fps     # just some fields, one per line
    dayz-vr-ctl.py watch --interval 0.5     # repeating one-line summary
    dayz-vr-ctl.py tunables                 # current tunable values
    dayz-vr-ctl.py set stereo.hmd_mouse_yaw_scale -300
    dayz-vr-ctl.py recenter
    dayz-vr-ctl.py haptic
    dayz-vr-ctl.py action UAMoveForward 1
    dayz-vr-ctl.py snapshot turned-left     # save state to build/snapshots/<time>-turned-left.json
    dayz-vr-ctl.py compare                  # yaw/pitch deltas between the last two snapshots
    dayz-vr-ctl.py calibrate --min-degrees 20   # turn your head: camera/HMD yaw ratio + aim error
"""

from __future__ import annotations

import argparse
import json
import math
import socket
import sys
import time
from datetime import datetime
from pathlib import Path
from typing import Any

DEFAULT_PORT = 48621
SNAPSHOT_DIR = Path(__file__).resolve().parent.parent / "build" / "snapshots"


class DebugClient:
    def __init__(self, host: str, port: int, timeout: float) -> None:
        self._socket = socket.create_connection((host, port), timeout=timeout)
        self._reader = self._socket.makefile("r", encoding="utf-8", newline="\n")

    def close(self) -> None:
        self._reader.close()
        self._socket.close()

    def request(self, line: str) -> Any:
        self._socket.sendall((line + "\n").encode("utf-8"))
        reply = self._reader.readline()
        if not reply:
            raise ConnectionError("the plugin closed the connection")
        return json.loads(reply)


def degrees(radians: float) -> float:
    return math.degrees(radians)


def summary(state: dict[str, Any]) -> str:
    hand = state["right_hand"]
    return (
        f"fps={state['host_fps']:5.1f} state={state['session_state']} "
        f"focus={'y' if state['window_focused'] else 'n'} "
        f"gui={'y' if state['gui_cursor_mode'] else 'n'} eye={state['rendered_eye']} "
        f"yaw={degrees(state['hmd_yaw']):7.1f} pitch={degrees(state['hmd_pitch']):6.1f} "
        f"roll={degrees(state['hmd_roll']):6.1f} "
        f"pos=({state['hmd_position'][0]:.3f},{state['hmd_position'][1]:.3f},{state['hmd_position'][2]:.3f}) "
        f"mouse=({state['pending_mouse_x']:.1f},{state['pending_mouse_y']:.1f}) "
        f"aimerr=({degrees(state.get('aim_yaw_error', 0.0)):+.1f},{degrees(state.get('aim_pitch_error', 0.0)):+.1f}) "
        f"gain=({state.get('aim_yaw_gain', 0.0):.0f},{state.get('aim_pitch_gain', 0.0):.0f}) "
        f"aimR={'ok' if hand['aim_valid'] else '--'} presents={state['present_count']}"
    )


def camera_yaw(direction: list[float]) -> float:
    """Yaw in degrees of a game-space direction vector, same convention as hmd_yaw."""
    return math.degrees(math.atan2(direction[0], direction[2]))


def wrap_degrees(value: float) -> float:
    return (value + 180.0) % 360.0 - 180.0


def save_snapshot(state: dict[str, Any], label: str) -> Path:
    SNAPSHOT_DIR.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now().astimezone().strftime("%Y%m%d-%H%M%S")
    path = SNAPSHOT_DIR / f"{stamp}-{label}.json"
    path.write_text(json.dumps(state, indent=2) + "\n", encoding="utf-8")
    return path


def compare_snapshots(before: dict[str, Any], after: dict[str, Any]) -> str:
    hmd_yaw = wrap_degrees(degrees(after["hmd_yaw"]) - degrees(before["hmd_yaw"]))
    hmd_pitch = degrees(after["hmd_pitch"]) - degrees(before["hmd_pitch"])
    cam_yaw = wrap_degrees(camera_yaw(after["native_camera_direction"]) - camera_yaw(before["native_camera_direction"]))
    ratio = cam_yaw / hmd_yaw if abs(hmd_yaw) > 1.0 else float("nan")
    lines = [
        f"HMD yaw:    {degrees(before['hmd_yaw']):7.1f} -> {degrees(after['hmd_yaw']):7.1f}  delta {hmd_yaw:+.1f} deg",
        f"HMD pitch:  {degrees(before['hmd_pitch']):7.1f} -> {degrees(after['hmd_pitch']):7.1f}  delta {hmd_pitch:+.1f} deg",
        f"HMD roll:   {degrees(before['hmd_roll']):7.1f} -> {degrees(after['hmd_roll']):7.1f}",
        f"camera yaw: {camera_yaw(before['native_camera_direction']):7.1f} -> {camera_yaw(after['native_camera_direction']):7.1f}  delta {cam_yaw:+.1f} deg",
        f"camera/HMD yaw ratio: {ratio:.3f}   (1.000 = head turn matches game turn)",
        f"focused: before={before['window_focused']} after={after['window_focused']}",
    ]
    return "\n".join(lines)


def calibrate(client: "DebugClient", seconds: float, min_degrees: float, interval: float) -> int:
    """Measure how far the game camera follows a head turn.

    Takes a reference state, waits until the HMD yaw has moved by min_degrees (or the
    time is up), then prints the snapshot comparison plus the aim loop's remaining
    error. Ratio 1.0 means the closed loop keeps the camera on the head; a ratio well
    below 1 while the error stays large means the loop is starved (focus lost, frame
    rate too low for the per-frame turn cap).
    """
    tunables = client.request("tunables")
    controller_aim = float(tunables.get("stereo.controller_aim", 0.0)) >= 0.5
    if controller_aim:
        print("note: stereo.controller_aim=1, the camera follows the right controller, not the head;"
              " the yaw ratio below is not meaningful, judge by the aim error "
              "(set stereo.controller_aim 0 to calibrate head aim)")
        seconds = min(seconds, 3.0)
    before = client.request("get")
    if not before.get("window_focused", False):
        print("warning: DayZ is not focused; mouse injection is paused while unfocused")
    print(f"reference: {summary(before)}")
    print(f"turn your head by at least {min_degrees:.0f} degrees (waiting up to {seconds:.0f} s)")
    deadline = time.monotonic() + seconds
    after = before
    while time.monotonic() < deadline:
        time.sleep(interval)
        after = client.request("get")
        moved = abs(wrap_degrees(degrees(after["hmd_yaw"]) - degrees(before["hmd_yaw"])))
        if moved >= min_degrees:
            break
    else:
        moved = abs(wrap_degrees(degrees(after["hmd_yaw"]) - degrees(before["hmd_yaw"])))
        print(f"timeout: head moved only {moved:.1f} degrees")
    # Let the loop settle, then take the final sample.
    time.sleep(max(interval, 0.5))
    after = client.request("get")
    print(compare_snapshots(before, after))
    yaw_error = degrees(after.get("aim_yaw_error", 0.0))
    pitch_error = degrees(after.get("aim_pitch_error", 0.0))
    print(f"aim error now: yaw {yaw_error:+.1f} deg, pitch {pitch_error:+.1f} deg; "
          f"gain yaw {after.get('aim_yaw_gain', 0.0):.0f} pitch {after.get('aim_pitch_gain', 0.0):.0f} counts/rad")
    save_snapshot(before, "calibrate-before")
    save_snapshot(after, "calibrate-after")
    settled = abs(yaw_error) < 3.0 and abs(pitch_error) < 3.0
    if controller_aim:
        ok = settled
        print("result: " + ("camera follows the controller (aim error under 3 deg)" if ok else "camera NOT settled on the controller"))
    else:
        ok = settled and moved >= min_degrees
        print("result: " + ("camera follows the head (aim error under 3 deg)" if ok else "camera NOT settled on the head"))
    return 0 if ok else 1


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT, help=f"[debug] port from the ini (default {DEFAULT_PORT})")
    parser.add_argument("--timeout", type=float, default=3.0)
    commands = parser.add_subparsers(dest="command", required=True)
    get = commands.add_parser("get", help="print the state, or selected fields")
    get.add_argument("fields", nargs="*")
    watch = commands.add_parser("watch", help="print a one-line summary repeatedly")
    watch.add_argument("--interval", type=float, default=1.0)
    watch.add_argument("--count", type=int, default=0, help="stop after this many samples (0 = until Ctrl-C)")
    commands.add_parser("tunables", help="list tunables and their current values")
    setter = commands.add_parser("set", help="change a tunable for the running game")
    setter.add_argument("name")
    setter.add_argument("value", type=float)
    commands.add_parser("recenter", help="recapture the HMD yaw and position centre")
    commands.add_parser("haptic", help="send a test vibration to the right controller")
    action = commands.add_parser("action", help="force a DayZ input action through the engine hooks (test aid)")
    action.add_argument("name", help="action name, e.g. UAMoveForward, UAFire, UAGetOver")
    action.add_argument("value", type=float, help="0 releases; > 0.5 also holds the digital state")
    commands.add_parser("dump-eyes", help="write dayz_openxr_eye0/1.bmp beside DayZ_x64.exe")
    commands.add_parser("ping")
    snapshot = commands.add_parser("snapshot", help="save the state to build/snapshots and compare with the previous one")
    snapshot.add_argument("label", nargs="?", default="snapshot")
    calibration = commands.add_parser("calibrate", help="measure the camera/HMD yaw ratio over a head turn")
    calibration.add_argument("--seconds", type=float, default=20.0, help="how long to wait for the head turn")
    calibration.add_argument("--min-degrees", type=float, default=20.0, help="head yaw change that ends the wait")
    calibration.add_argument("--interval", type=float, default=0.25)
    compare = commands.add_parser("compare", help="compare two saved snapshots (default: the last two)")
    compare.add_argument("before", nargs="?")
    compare.add_argument("after", nargs="?")
    args = parser.parse_args(argv)

    if args.command == "compare":
        files = sorted(SNAPSHOT_DIR.glob("*.json"))
        before = Path(args.before) if args.before else (files[-2] if len(files) >= 2 else None)
        after = Path(args.after) if args.after else (files[-1] if files else None)
        if before is None or after is None:
            print("need two snapshots to compare", file=sys.stderr)
            return 2
        print(f"{before.name} -> {after.name}")
        print(compare_snapshots(json.loads(before.read_text()), json.loads(after.read_text())))
        return 0

    try:
        client = DebugClient(args.host, args.port, args.timeout)
    except OSError as error:
        print(f"cannot connect to {args.host}:{args.port}: {error}. Is DayZ running with [debug] enabled=true?", file=sys.stderr)
        return 2
    try:
        if args.command == "get":
            state = client.request("get")
            if args.fields:
                for field in args.fields:
                    print(f"{field}={json.dumps(state.get(field))}")
            else:
                print(json.dumps(state, indent=2))
        elif args.command == "watch":
            samples = 0
            while args.count == 0 or samples < args.count:
                print(summary(client.request("get")), flush=True)
                samples += 1
                if args.count == 0 or samples < args.count:
                    time.sleep(args.interval)
        elif args.command == "tunables":
            for name, value in client.request("tunables").items():
                print(f"{name}={value}")
        elif args.command == "set":
            reply = client.request(f"set {args.name} {args.value!r}")
            print(json.dumps(reply))
            return 0 if reply.get("ok") else 1
        elif args.command == "recenter":
            print(json.dumps(client.request("recenter")))
        elif args.command == "haptic":
            reply = client.request("haptic")
            print(json.dumps(reply))
            return 0 if reply.get("ok") else 1
        elif args.command == "action":
            reply = client.request(f"action {args.name} {args.value!r}")
            print(json.dumps(reply))
            return 0 if reply.get("ok") else 1
        elif args.command == "calibrate":
            return calibrate(client, args.seconds, args.min_degrees, args.interval)
        elif args.command == "dump-eyes":
            print(json.dumps(client.request("dump_eyes")))
        elif args.command == "ping":
            print(json.dumps(client.request("ping")))
        elif args.command == "snapshot":
            previous = sorted(SNAPSHOT_DIR.glob("*.json"))
            state = client.request("get")
            path = save_snapshot(state, args.label)
            print(f"saved {path}")
            print(summary(state))
            if previous:
                print(f"vs {previous[-1].name}:")
                print(compare_snapshots(json.loads(previous[-1].read_text()), state))
    except KeyboardInterrupt:
        pass
    finally:
        client.close()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
