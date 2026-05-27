# demo/kill.tape — `lport kill <port>` demo with update-available notice
# Run: cd <repo> && vhs demo/kill.tape
#
# Spawns a throwaway python http.server, kills it, and shows the
# update-available prompt (cache is primed to a higher version so the
# notice appears deterministically).

Output demo/kill.gif

Set Shell "bash"
Set FontSize 13
Set Width 1100
Set Height 320
Set Theme "Dracula"
Set TypingSpeed 70ms
Set Framerate 24
Set LoopOffset 5%
Set PlaybackSpeed 1.4

Env PS1 "$ "
Env TERM "xterm-256color"

Hide
# Prime cache so update notice prints under the kill line.
Type "sh demo/_set-cache.sh 0.7.0"
Enter
# `setsid -f` detaches python from this shell session so bash never
# echoes `Terminated` when lport sends SIGTERM.
Type "setsid -f python3 -m http.server 47829 >/dev/null 2>&1"
Enter
Sleep 1500ms
Type "clear"
Enter
Show

Sleep 600ms
Type "lport kill 47829"
Enter
Sleep 4s

# Restore cache to current so the demo leaves no surprise on disk.
Hide
Type "sh demo/_set-cache.sh 0.6.0"
Enter
Show
