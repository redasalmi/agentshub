# Opens the settings, changes notifications, closes them, and reopens them
# to change the theme. Covers: settings intents reaching the workspace,
# persistence, the close shortcut and the theme cards.

project="$AHV_WORK/project"
seed_project "$project"

setting() { sql "SELECT value FROM settings WHERE key = '$1'"; }

app_start
key ctrl+1
key ctrl+comma
shot opened

# The notifications are below the fold; the switches are on the right.
xdotool mousemove 640 400
for _ in 1 2 3 4 5 6 7 8 9 10; do xdotool click 5; sleep 0.1; done
shot notifications
click 1090 247
wait_until 5 "popups off" [ "$(setting notification_popups)" = "0" ]
click 1090 316
wait_until 5 "sound off" [ "$(setting notification_sound)" = "0" ]
shot changed

key ctrl+comma
shot closed

key ctrl+comma
xdotool mousemove 640 400
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do xdotool click 4; sleep 0.1; done
shot top
click 1055 250
wait_until 5 "theme changed" [ "$(setting theme)" = "primer" ]
shot theme
