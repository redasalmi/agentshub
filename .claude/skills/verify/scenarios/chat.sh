# Opens a chat, shows and hides the project shell, and opens the chat menu.
# Covers: the chat sheet, the shell panel, and the chat actions.

project="$AHV_WORK/project"
seed_project "$project"

app_start
key ctrl+1
key ctrl+shift+n
key Return
wait_until 15 "fake claude to start" cli_started claude
shot chat

click 1189 84
shot shell
click 1189 84
shot shell-hidden

click 1240 84
shot menu
key Escape
