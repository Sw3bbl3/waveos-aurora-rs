# Terminal command reference
Programs are stored in /System/Bin. Run `help` in Terminal for shell commands; individual tools print usage when arguments are missing or invalid.

| Task | Commands |
|---|---|
| Files | ls, cat, cp, mv, rm, mkdir, touch |
| Text | echo, grep, wc |
| Processes | ps, kill |
| Storage | df, sync |
| System | uname, uptime, date, mem, neofetch, cpuinfo, dmesg |
| Devices | lspci, lsusb, battery |
| Network | ifconfig, nslookup, ping, fetch |
| Sound | play, volume |
| Apps | constellation, gina-check, gina-demo |

## Practice safely
1. Create a temporary directory with `mkdir /Documents/practice`.
2. Write `echo hello > /Documents/practice/message.txt`.
3. Count it with `cat /Documents/practice/message.txt | wc`.
4. Copy it with `cp /Documents/practice/message.txt /Documents/practice/copy.txt`.

Expected result: two readable files. Redirection with > replaces a file; >> appends. `rm` removes directly rather than using Files’ Trash. Verify paths before destructive commands.

## Troubleshooting
Command names and paths are case-sensitive. Programs may implement only a subset of familiar Unix flags. Do not assume shell scripting, package managers, or native Rust tools are present.

## Shell controls
Use `cd DIR` to change directories, `history` to review commands, `clear` or Ctrl+L to clear the view, and `exit` to close Terminal. `theme light` and `theme dark` select appearance. `open Learn` launches an app; `open /Documents/practice/message.txt` opens a file. `shutdown` and `reboot` affect the whole system.

## More examples
```sh
ls /Documents | grep txt
ps
uname
ifconfig
nslookup example.com
fetch https://example.com
sync
```
Network examples require a configured adapter and a reachable network. `a && b` runs b after success; `a || b` runs b after failure; `a; b` runs both. Ctrl+C stops the foreground program. Quote paths containing spaces. Use `kill PID` only after identifying the intended process with `ps`.
