# Opens a chat's folder in an editor from its header, picks another from the
# menu, and checks the setting. Covers: finding the installed editors, the
# header's editor button and menu, launching, and the Code editor setting.

project="$AHV_WORK/project"
seed_project "$project"
seed_editor dev.zed.Zed '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect width="16" height="16" rx="3" fill="#e5484d"/></svg>'
seed_editor code

# What a launcher such as scripts/try-dev.sh hands back to editors.
export AGENTSHUB_EDITOR_XDG_DATA_HOME="$AHV_WORK/restored"
export AGENTSHUB_EDITOR_DBUS_SESSION_BUS_ADDRESS=
app_start
key ctrl+1
key ctrl+shift+n
key Return
wait_until 15 "fake claude to start" cli_started claude
shot header

click 1120 86
wait_until 5 "zed to open the project" grep -qxF "dev.zed.Zed|$project|$project" "$EDITOR_LOG"
zed_env="$AHV_OUT/editor-env-dev.zed.Zed"
grep -qxF "XDG_DATA_HOME=$AHV_WORK/restored" "$zed_env" || fail "the editor didn't get XDG_DATA_HOME back"
grep -q '^DBUS_SESSION_BUS_ADDRESS=' "$zed_env" && fail "the editor kept the app's session bus"
grep -q '^AGENTSHUB_EDITOR_' "$zed_env" && fail "AGENTSHUB_EDITOR_ variables reached the editor"

editor_setting() { [ "$(sql "SELECT value FROM settings WHERE key = 'editor'")" = "$1" ]; }

click 1148 86
shot editor-menu
click 1113 173
wait_until 5 "VS Code to be saved as the editor" editor_setting vscode
shot header-vscode
grep -q '^code|' "$EDITOR_LOG" && fail "picking VS Code in the menu opened it"
click 1120 86
wait_until 5 "VS Code to open the project" grep -qxF "code|$project|$project" "$EDITOR_LOG"

key ctrl+comma
xdotool mousemove 640 400
for _ in 1 2 3 4 5; do xdotool click 5; sleep 0.1; done
shot settings
click 1067 562
shot settings-menu
click 1060 619
wait_until 5 "Zed to be saved as the editor" editor_setting zed
shot settings-zed
assert_eq "$(grep -c '^dev.zed.Zed|' "$EDITOR_LOG")" 1 "editors opened from the settings"
