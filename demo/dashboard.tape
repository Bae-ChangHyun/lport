# demo/dashboard.tape — `lport` dashboard demo
# Run: cd <repo> && vhs demo/dashboard.tape
#
# Shows the default dashboard. The output on the recorder's box is taller
# than one screenful, so we cap it with `| head -22` to keep the GIF
# focused on the [ project ] grouping behaviour without overwhelming the
# viewer.

Output demo/dashboard.gif

Set Shell "bash"
Set FontSize 13
Set Width 1500
Set Height 460
Set Theme "Dracula"
Set TypingSpeed 70ms
Set Framerate 24
Set LoopOffset 5%
Set PlaybackSpeed 1.4

Env PS1 "$ "
Env TERM "xterm-256color"

# Mark cache as current so no update notice steals attention here.
Hide
Type "sh demo/_set-cache.sh 0.6.0"
Enter
Type "clear"
Enter
Show

Sleep 600ms
Type "lport | head -16"
Enter
Sleep 6s
