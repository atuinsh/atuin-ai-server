# The README demo. Record it with res/demo/record.sh, which sets up $DEMO.

Output res/demo.gif

Set Shell zsh
Set FontSize 20
Set Width 1280
Set Height 720
Set Padding 24
Set WindowBar Colorful
Set Theme "Catppuccin Mocha"
Set TypingSpeed 45ms
Set WaitTimeout 20s

Hide
Type 'exec env HOME="$DEMO/home" ZDOTDIR="$DEMO" ATUIN_HOME="$DEMO/atuin" PATH="$DEMO/bin:$PATH" zsh'
Enter
Sleep 500ms
Type "cd ~/api && clear"
Enter
Sleep 500ms
Show

Sleep 1s
Type "?"
Sleep 800ms
Type "find files over 50MB I changed this week"
Sleep 500ms
Enter
Wait+Screen /Suggested command/
Sleep 2s
Enter
Sleep 3s

Ctrl+L
Sleep 800ms
Type "?"
Sleep 800ms
Type "rerun yesterday's docker build, but for arm64"
Sleep 500ms
Enter
Wait+Screen /Always allow/
Sleep 1.5s
Enter
Wait+Screen /Suggested command/
Sleep 3s
Tab
Sleep 3s
