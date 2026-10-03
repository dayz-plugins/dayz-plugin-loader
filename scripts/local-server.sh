#!/usr/bin/env bash
# Vanilla DayZ dedicated server on this machine for testing the VR proxy without
# touching the user's real server. Runs inside the same container image the
# Pterodactyl egg uses (ghcr.io/parkervcp/games:dayz) with loopback-only published ports, from a
# private copy of Steam's "DayZ Server" (app 223350) install, with BattlEye patched
# out by pterodactyl-eggs/dayz-standalone/patch_be.pl so a -nobe client can join.
#
#   scripts/local-server.sh setup    copy the Steam install, patch BattlEye, write a maximally
#                                    lenient serverDZ.cfg (no signatures, no same-build, no
#                                    mod equality, no shot validation, no bans), empty ban list
#   scripts/local-server.sh start    start the server container (port 2302, query 2305)
#   scripts/local-server.sh stop     stop it
#   scripts/local-server.sh status   container state and the last log lines
#   scripts/local-server.sh logs     follow the server log (Ctrl-C to stop following)
#
# Join from the client with: scripts/run-dayz-direct.sh --sim -- -connect=127.0.0.1 -port=2302
# Environment overrides: DAYZ_SERVER_SRC, EGGS_DIR, SERVER_IMAGE, SERVER_PORT, SERVER_QUERY_PORT.
set -euo pipefail
IFS=$'\n\t'

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
project_dir="$(dirname -- "$script_dir")"
server_dir="$project_dir/build/local-server"
server_src="${DAYZ_SERVER_SRC:-/run/media/system/Data/Games/Steam/steamapps/common/DayZServer}"
eggs_dir="${EGGS_DIR:-/run/media/system/Data/Projects/pterodactyl-eggs/dayz-standalone}"
image="${SERVER_IMAGE:-ghcr.io/parkervcp/games:dayz}"
port="${SERVER_PORT:-2302}"
[[ "$port" =~ ^[0-9]+$ ]] || { echo "SERVER_PORT must be numeric" >&2; exit 2; }
port=$((10#$port))
query_port="${SERVER_QUERY_PORT:-$((port + 3))}"
[[ "$query_port" =~ ^[0-9]+$ ]] || { echo "SERVER_QUERY_PORT must be numeric" >&2; exit 2; }
query_port=$((10#$query_port))
(( port >= 1024 && port <= 65532 && query_port >= 1024 && query_port <= 65535 )) || {
  echo "server ports must be in 1024..65535 (game port at most 65532)" >&2; exit 2;
}
container_name="dayz-vr-local-server"

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

cmd_setup() {
  [[ -f "$server_src/DayZServer" ]] || die "DayZ Server (app 223350) is not installed at $server_src"
  [[ -f "$eggs_dir/patch_be.pl" ]] || die "patch_be.pl not found in $eggs_dir"
  command -v podman >/dev/null || die "podman is required"
  if podman container exists "$container_name"; then
    local running
    running="$(podman inspect --format '{{.State.Running}}' "$container_name")"
    [[ "$running" == false ]] || die "stop the local server before setup; refusing to overwrite a running instance"
  fi
  mkdir -p "$server_dir"
  say "copying the server install to $server_dir (first run copies ~3.8 GB)"
  rsync -a --delete --exclude serverprofile --exclude serverDZ.cfg \
    --exclude @DayZVR_Server --exclude .steam "$server_src/" "$server_dir/"
  mkdir -p "$server_dir/serverprofile" "$server_dir/.steam/sdk64"
  cp -f "$server_dir/steamclient.so" "$server_dir/.steam/sdk64/steamclient.so"
  say "patching BattlEye out of the server binary"
  perl "$eggs_dir/patch_be.pl" "$server_dir/DayZServer"
  # The Steam install ships a populated ban.txt; the local server bans nobody.
  printf '// Local test server: no bans (scripts/local-server.sh setup rewrites this file).\n' > "$server_dir/ban.txt"
  : > "$server_dir/whitelist.txt"
  cat > "$server_dir/serverDZ.cfg" <<CFG
hostname = "DayZ-VR local test server";
password = "";
passwordAdmin = "";
enableWhitelist = 0;
maxPlayers = 4;
// As lenient as the engine allows: this server exists only for the local VR test
// client, which runs with a dxgi proxy, patched inputs and (optionally) loose files.
verifySignatures = 0;
forceSameBuild = 0;
equalModRequired = 0;
shotValidation = 0;
disableBanlist = true;
disablePrioritylist = true;
disableMultiAccountMitigation = true;
pingWarning = 1000;
pingCritical = 2000;
MaxPing = 5000;
disablePersonalLight = 0;
disableVoN = 1;
disable3rdPerson = 0;
disableCrosshair = 0;
// Fixed noon start (daylight for every rendering session); real-time progression.
serverTime = "2026/6/21/12/00";
serverTimeAcceleration = 1;
serverNightTimeAcceleration = 1;
serverTimePersistent = 0;
guaranteedUpdates = 1;
loginQueueConcurrentPlayers = 5;
loginQueueMaxPlayers = 500;
instanceId = 1;
storageAutoFix = 1;
lootHistory = 1;
storeHouseStateDisabled = false;
allowFilePatching = 1;
// Mission cfggameplay.json (written by setup: easy stamina/shock/drowning, no base damage).
enableCfgGameplayFile = 1;
steamQueryPort = $query_port;
enableDebugMonitor = 1;
logAverageFps = 60;
logMemory = 60;
logPlayers = 60;
logFile = "server_console.log";
BattlEye = 0;
class Missions
{
    class DayZ
    {
        template = "dayzOffline.chernarusplus";
    };
};
CFG
  write_easy_mission
  say "pulling $image if needed"
  podman image exists "$image" || podman pull "$image"
  say "setup complete"
}

# Lowest difficulty the mission's config files allow (rsync restores the vanilla files
# on every setup, so this runs after it): cfggameplay.json (stamina never limits, shock
# refills fast, drowning slow, no base/container damage, no respawn dialog, mild
# temperatures, 3D map and player position on the map) and globals.xml (no infected, no
# food decay, pristine loot, short login/logout timers). Hunger, thirst and blood-loss
# rates are script constants (PlayerConstants), not config, and are left alone; the
# "loadout" and "heal" commands of @DayZVR_Server cover the rest.
write_easy_mission() {
  local mission="$server_dir/mpmissions/dayzOffline.chernarusplus"
  if [[ ! -f "$mission/cfggameplay.json" || ! -f "$mission/db/globals.xml" ]]; then
    say "warning: mission files missing in $mission; easy-mode mission edit skipped"
    return 0
  fi
  say "writing the easy-mode mission files"
  python3 - "$mission" <<'PY'
import json, re, sys
from pathlib import Path
mission = Path(sys.argv[1])
cfg_path = mission / "cfggameplay.json"
cfg = json.loads(cfg_path.read_text(encoding="utf-8"))
cfg["GeneralData"].update({"disableBaseDamage": True, "disableContainerDamage": True,
                           "disableRespawnDialog": True, "disableRespawnInUnconsciousness": True})
player = cfg["PlayerData"]
player["disablePersonalLight"] = False
player["StaminaData"].update({"sprintStaminaModifierErc": 0.05, "sprintStaminaModifierCro": 0.05,
    "staminaWeightLimitThreshold": 60000.0, "staminaMax": 100.0, "staminaKgToStaminaPercentPenalty": 0.0,
    "staminaMinCap": 100.0, "sprintSwimmingStaminaModifier": 0.05, "sprintLadderStaminaModifier": 0.05,
    "meleeStaminaModifier": 0.05, "obstacleTraversalStaminaModifier": 0.05, "holdBreathStaminaModifier": 0.05})
player["ShockHandlingData"].update({"shockRefillSpeedConscious": 50.0, "shockRefillSpeedUnconscious": 50.0,
    "allowRefillSpeedModifier": True})
player["MovementData"]["allowStaminaAffectInertia"] = False
player["DrowningData"].update({"staminaDepletionSpeed": 0.1, "healthDepletionSpeed": 0.1, "shockDepletionSpeed": 0.1})
cfg["WorldsData"]["environmentMinTemps"] = [18] * 12
cfg["WorldsData"]["environmentMaxTemps"] = [24] * 12
cfg["WorldsData"]["wetnessWeightModifiers"] = [1.0] * 5
for group in cfg["BaseBuildingData"]["HologramData"], cfg["BaseBuildingData"]["ConstructionData"]:
    for key, value in group.items():
        if isinstance(value, bool):
            group[key] = True
cfg["UIData"]["use3DMap"] = True
cfg["MapData"].update({"ignoreMapOwnership": True, "ignoreNavItemsOwnership": True,
                       "displayPlayerPosition": True, "displayNavInfo": True})
cfg_path.write_text(json.dumps(cfg, indent="\t") + "\n", encoding="utf-8")

globals_path = mission / "db" / "globals.xml"
text = globals_path.read_text(encoding="utf-8")
for name, value in {"ZombieMaxCount": "0", "AnimalMaxCount": "200", "FoodDecay": "0", "LootDamageMin": "0.0",
                    "LootDamageMax": "0.0", "TimeLogin": "5", "TimeLogout": "5", "TimePenalty": "0",
                    "TimeHopping": "0", "IdleModeStartup": "0"}.items():
    text, count = re.subn(rf'(<var name="{name}" type="\d+" value=")[^"]*(")', rf"\g<1>{value}\g<2>", text)
    if count != 1:
        raise SystemExit(f"globals.xml: {name} not found")
globals_path.write_text(text, encoding="utf-8")

print("cfggameplay.json and db/globals.xml written")
PY
}

# Prints serverDZ.cfg and the difficulty values the mission files carry, so a stale
# or unexpected setting is visible on every start.
print_active_config() {
  say "active $server_dir/serverDZ.cfg (comments stripped)"
  grep -vE '^[[:space:]]*(//|$)' "$server_dir/serverDZ.cfg" | sed 's/^/    /'
  local mission="$server_dir/mpmissions/dayzOffline.chernarusplus"
  if [[ -f "$mission/cfggameplay.json" ]]; then
    say "active $mission/cfggameplay.json (GeneralData, StaminaData, ShockHandlingData)"
    python3 - "$mission/cfggameplay.json" <<'PY' | sed 's/^/    /'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
for section in ("GeneralData", "StaminaData", "ShockHandlingData", "DrowningData"):
    for key, value in data.get(section, {}).items():
        print(f"{section}.{key} = {value}")
PY
  fi
  if [[ -f "$mission/db/globals.xml" ]]; then
    say "active $mission/db/globals.xml"
    grep -oE 'name="[^"]+" type="[0-9]+" value="[^"]*"' "$mission/db/globals.xml" \
      | sed -E 's/name="([^"]+)" type="[0-9]+" value="([^"]*)"/    \1 = \2/'
  fi
}

cmd_start() {
  [[ -f "$server_dir/DayZServer" && -f "$server_dir/serverDZ.cfg" ]] || die "run 'setup' first"
  if podman container exists "$container_name"; then
    podman rm -f "$container_name" >/dev/null
  fi
  # The test-command mod (enforce/DayZVR_Server, deployed by build.sh --deploy) lets
  # scripts/dayz-cmd.sh spawn gear/vehicles and teleport the connected player.
  local server_mods=""
  if [[ -d "$server_dir/@DayZVR_Server" ]]; then
    server_mods="-serverMod=@DayZVR_Server"
    say "loading server mod @DayZVR_Server"
  fi
  print_active_config
  say "starting $container_name on UDP $port (log: scripts/local-server.sh logs)"
  # --userns=keep-id keeps the host uid so the image's 'container' user (uid 1000)
  # owns the mounted files. Publish game/Steam/query UDP only on loopback, since
  # this unsigned test server is intended solely for the local test client.
  podman run -d --name "$container_name" --network bridge --userns=keep-id \
    -p "127.0.0.1:$port-$((port + 2)):$port-$((port + 2))/udp" \
    -p "127.0.0.1:$query_port:$query_port/udp" \
    -v "$server_dir:/home/container:Z" -w /home/container --entrypoint /bin/bash "$image" \
    -c "./DayZServer -config=serverDZ.cfg -port=$port -profiles=serverprofile -BEpath=battleye -filePatching -scriptDebug=true -dologs -adminlog -limitFPS=60 $server_mods" \
    >/dev/null
  cmd_status
}

cmd_stop() {
  if podman container exists "$container_name"; then
    podman stop -t 20 "$container_name" >/dev/null && say "stopped"
    podman rm -f "$container_name" >/dev/null
  else
    say "server container is not running"
  fi
}

cmd_status() {
  if podman container exists "$container_name"; then
    podman ps -a --filter "name=$container_name" --format 'status: {{.Status}}'
    podman logs --tail 8 "$container_name" 2>&1 | cut -c1-200
  else
    say "server container does not exist"
  fi
}

case "${1:-}" in
  setup) cmd_setup ;;
  start) cmd_start ;;
  stop) cmd_stop ;;
  restart) cmd_stop; cmd_start ;;
  status) cmd_status ;;
  logs) podman logs -f "$container_name" ;;
  -h | --help | "") sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; [[ -n "${1:-}" ]] || exit 2 ;;
  *) die "unknown command: $1" ;;
esac
