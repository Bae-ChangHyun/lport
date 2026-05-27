# demo/info.tape — `lport info <port>` single-port detail demo
# Run: cd <repo> && vhs demo/info.tape
#
# Spawns a throwaway python http.server on a clean cwd so the detail
# block reads as a self-contained example.

Output demo/info.gif

Set Shell "bash"
Set FontSize 13
Set Width 1100
Set Height 440
Set Theme "Dracula"
Set TypingSpeed 70ms
Set Framerate 24
Set LoopOffset 5%
Set PlaybackSpeed 1.4

Env PS1 "$ "
Env TERM "xterm-256color"

Hide
Type "sh demo/_set-cache.sh 0.6.0"
Enter
Type "rm -rf /tmp/demo-server && mkdir -p /tmp/demo-server"
Enter
Type "(cd /tmp/demo-server && setsid -f python3 -m http.server 47829 >/dev/null 2>&1)"
Enter
Sleep 1500ms
Type "clear"
Enter
Show

Sleep 600ms
Type "lport info 47829"
Enter
Sleep 5s

# Cleanup
Hide
Type "pkill -f 'http.server 47829' 2>/dev/null; rm -rf /tmp/demo-server"
Enter
Show
