# Opens a chat, shows and hides the project shell, and opens the chat menu.
# Covers: the chat sheet, the shell panel, and the chat actions. An editor is
# installed so the header looks the same on every machine.

project="$AHV_WORK/project"
seed_project "$project"
seed_editor dev.zed.Zed

app_start
key ctrl+1
key ctrl+shift+n
key Return
wait_until 15 "fake claude to start" cli_started claude
shot chat

click 1197 86
shot shell
click 1197 86
shot shell-hidden

click 1246 86
shot menu
key Escape

click 1030 25
shot running-menu
key Escape

key ctrl+shift+k
shot search
key Escape
