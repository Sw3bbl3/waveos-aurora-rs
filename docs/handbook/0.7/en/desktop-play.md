# Wave Play
Play with a game controller: your games and apps in large rows, full screen.

## Start Wave Play
1. Connect a USB game controller: Xbox, PlayStation, Switch Pro, or any HID gamepad.
2. A notice confirms the connection, and the desktop turns into Wave Play: the windows step back, the bar and the dock slide away, and rows of games and apps rise in.
3. Without a controller, choose Wave Play in the WaveOS menu, or search for Wave Play with Super+Space.

To stop Wave Play opening by itself, turn off **Open when a controller connects** in Settings → Wave Play. A controller then shows a notice instead; press A to enter.

## Find your way around
| Button | What it does |
| --- | --- |
| D-pad or left stick | Move between tiles; hold to move faster |
| A | Open the tile (or return to what is running) |
| Y | Search with an on-screen strip of letters |
| LB and RB | Jump to the row above or below |
| Menu | Open the quick menu |
| View | Go back to the desktop |
| Home | The quick menu, from anywhere |

On PlayStation controllers, A is the cross; on Nintendo controllers, A is the right-hand button, as on the console. The hints at the bottom show your controller's own symbols.

The rows are **Continue** (what you started recently, and what is still running), **Games**, **Apps** and **Settings**. Each row remembers where you were. Holding a direction at the end of a row stops there; pressing again goes round to the other end.

A keyboard also works: arrows move, Enter opens, Escape goes back, Tab searches and Super+Escape opens the quick menu.

## Play
Opening a tile grows it to fill the screen while the program starts. Games and apps started from Wave Play run full screen. Doom reads the controller directly: the left stick and d-pad move, the right trigger fires, A opens doors, the shoulders strafe, View shows the map and Menu opens Doom's menu. Programs that don't read controllers receive the d-pad as arrow keys, A as Enter and B as Escape.

## The quick menu
Press the home button (or Super+Escape) over a game, Wave Play or the desktop. What is underneath dims and blurs, and a panel slides in:

- **Resume** closes the menu.
- **Wave Play Home** goes back to your rows; the game keeps running and waits in Continue.
- **Close** ends the game or app.
- **Volume** changes with left and right.
- **Go to Desktop** leaves Wave Play.
- **Power** offers sleep, restart and shut down.

The panel also shows the time, the battery, your controller, and how fast the screen is drawn (frames per second, and milliseconds a frame).

## Back to the desktop
Press View, choose the Desktop tile, or unplug the controller. Everything returns to exactly where it was: your windows, their order and the one in front. Pressing View again while the desktop is coming back returns to Wave Play from where the movement is.

## Settings
Settings → Wave Play has **Open when a controller connects**, **Sounds and vibration** (quiet clicks as you move, and a light vibration on controllers that have motors), and **Reduce motion**, which switches without the animated transformation. Reduce motion is shared with Accessibility.

## Troubleshooting
If a controller isn't recognized, check it in Settings → Game Controllers. Programs installed with the Linux packages appear under Games when their menu entry says they are games, and Steam appears when it is installed. Developers can add tiles by placing a `.play` file in `/Settings/Play/Library` (see the Wave Play notes in the WaveSDK guide).
