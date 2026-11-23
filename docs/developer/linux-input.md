# Input devices for Linux programs

Unmodified Linux games and toolkits (SDL2/SDL3, libinput, Steam's input
layer, Wine) find keyboards, pointers, touchpads and game controllers
through `/dev/input`, `/dev/hidraw*`, sysfs and udev. WaveOS provides all
of these, so a Linux program needs nothing WaveOS-specific (`waveos.h`'s
`wave_gamepad_*` calls remain for programs written for WaveOS).

## Where the code is

| Part | File |
|---|---|
| The devices: capabilities, state, readers, focus, grabs, key repeat, force-feedback effects, joystick views | `kernel/src/drivers/evdev/mod.rs` |
| Linux's event codes; HID usage and PS/2 scancode → key tables | `kernel/src/drivers/evdev/codes.rs` |
| How drivers describe and feed keyboards, pointers, touchpads | `kernel/src/drivers/evdev/sources.rs` |
| Controllers presented as `xpad`, `hid-playstation`, `hid-nintendo` do | `kernel/src/drivers/evdev/pad.rs` |
| Raw HID devices | `kernel/src/drivers/evdev/hidraw.rs` |
| `/dev/input/event*` (reads, writes, ioctls) | `kernel/src/compat/linux/input/evdev.rs` |
| `/dev/input/js*` | `kernel/src/compat/linux/input/joydev.rs` |
| `/dev/hidraw*` | `kernel/src/compat/linux/input/hidraw.rs` |
| `/dev/uinput` | `kernel/src/compat/linux/input/uinput.rs` |
| `/dev/input`, `/sys`, `/run/udev` as files | `kernel/src/compat/linux/input/tree.rs` |
| udev's properties, records and messages | `kernel/src/compat/linux/input/udev.rs` |
| `NETLINK_KOBJECT_UEVENT` sockets | `kernel/src/compat/linux/uevent.rs` |
| inotify | `kernel/src/compat/linux/inotify.rs` |
| Character devices in the file layer (`Special::Device`) | `kernel/src/compat/linux/chardev.rs` |

The native drivers feed both WaveOS's own input (`drivers::input`,
`drivers::gamepad`) and their Linux device: the PS/2 keyboard (as
`atkbd`: "AT Translated Set 2 keyboard", scancodes as `MSC_SCAN`), the PS/2
mouse and VMware pointer (as `psmouse` and `vmmouse`), USB HID keyboards,
mice, tablets and touchpads (as `hid-input` and `hid-multitouch`, named
"*manufacturer* *product*" like Linux's HID core), I²C HID touchpads and
mice, and every game controller (through `drivers::gamepad`, which calls
`evdev::pad`).

## Controllers

Each controller is shown as the Linux driver for it shows it, so SDL's
controller database (`gamecontrollerdb`, built into SDL) matches its GUID
(bus, vendor, product, version) and every button lands where the game
expects:

| Controller | Name | Buttons | Sticks | Triggers | D-pad | Version |
|---|---|---|---|---|---|---|
| Xbox 360 (`xpad`) | "Microsoft X-Box 360 pad" | `BTN_A`, `BTN_B`, `BTN_X` (west), `BTN_Y`, `TL`, `TR`, `SELECT`, `START`, `MODE`, `THUMBL/R` | ±32768, fuzz 16, flat 128 | `ABS_Z`/`ABS_RZ` 0–255 | hat | `bcdDevice` |
| Xbox One / Series (`xpad`) | xpad's names | same | same | 0–1023 | hat | `bcdDevice` |
| DualShock 4, DualSense (`hid-playstation`) | "Sony Interactive Entertainment Wireless Controller" … | positional `BTN_SOUTH/EAST/NORTH/WEST`, `TL2/TR2` too | 0–255 | 0–255 | hat | `bcdDevice \| 0x8000` |
| Switch Pro (`hid-nintendo`) | "Nintendo Switch Pro Controller" | positional, `BTN_Z` = Capture | ±32767, fuzz 250, flat 500 | buttons | hat | `bcdDevice` |
| Others | their own | as Xbox | as Xbox | 0–255 | hat | `bcdDevice` |

The keyboard-played controller (Super+G, `--virtual-gamepad`) is shown as
an Xbox 360 pad (`045e:028e`, v0114, under `/sys/devices/virtual`), as
Steam's virtual controllers are. Every controller with motors has force
feedback (`FF_RUMBLE`, periodic effects played as rumble, `FF_GAIN`; 16
effects): playing an effect calls `gamepad::rumble_async` with the
magnitudes scaled by the gain, for the effect's length times its count.
Effect delays and envelopes are ignored. A controller's joystick-API node
(`jsN`) numbers buttons and axes as Linux's `joydev` does (buttons from
`BTN_MISC` up, axes by code), with its default correction.

## Who may read what

- **Files.** Event, joystick and uinput nodes belong to the account
  logged in at the screen, `crw-rw----` (Linux gives them to the seat's
  user the same way, through logind's `uaccess`). Raw HID nodes of
  controllers too; those of keyboards and pointers belong to the system
  (`crw-------`), so no program can read typed keys from them.
- **Events.** A keyboard, pointer or touchpad's events reach a reader only
  while its process has the keyboard focus: its window is the focused one
  (not under the lock screen or Wave Play's overlay). Controllers follow
  Wave Play's rule (`gamepad::may_read`): the program whose window is in
  front, or anyone while a built-in app is. A reader that may not read
  sees the device at rest, and the switch is a report of its own: on losing
  the focus, releases for whatever was held (sticks centred, triggers let
  go, fingers lifted); on gaining it, the state as it is. `EVIOCGKEY` and
  `EVIOCGABS` agree with what the reader was told. Raw HID reports are
  dropped for a reader without the focus.
- **Bridges.** A window drawn by `wl-bridge` belongs to the bridge, so the
  bridge says which program it shows (`win_set_program`, from the Wayland
  client's `SO_PEERCRED`); that program gets the window's focus too. A
  program drawing nothing (KMS full screen) has no window and so never
  has the focus yet.
- **Grabs** (`EVIOCGRAB`) are exclusive among Linux readers only; WaveOS's
  own input keeps working, so a grabbed keyboard can't lock the user out.
- **Injection.** Keys and motion written to a `/dev/uinput` device go to
  WaveOS's input like a keyboard's, so to the focused window, as on Linux;
  `/dev/uinput` is therefore the logged-in account's only.

## Finding devices

- `/dev/input/event*`, `js*`, `/dev/input/by-id/*` (USB devices:
  `usb-<vendor>_<model>[_<serial>][-ifNN]-event-kbd|mouse|joystick`,
  `-joystick` for `js`) and `/dev/input/by-path/*` (`pci-…-usb-0:…:1.N-…`,
  `platform-i8042-serio-0-event-kbd`); virtual devices have neither, as on
  Linux.
- sysfs: `/sys/devices/…/input/inputN` (`name`, `phys`, `uniq`,
  `properties`, `modalias`, `id/*`, `capabilities/*`, `uevent`,
  `subsystem`, `device`) with `eventN` and `jsN` under it, USB devices,
  interfaces and HID devices above (`idVendor`, `manufacturer`,
  `bInterfaceNumber`, `HID_ID` …, `report_descriptor`), and
  `/sys/class/input`, `/sys/class/hidraw`, `/sys/bus/{input,hid,usb}/devices`
  links, and `/sys/dev/char/MAJOR:MINOR` for every node. Links are
  relative, as Linux's. `/sys` is shared with the display
  (`compat/linux/sysfs.rs`: `/sys/class/drm`, the PCI devices): the file
  layer asks `input::lookup`, `input::entries` and `input::link`, which
  answer from the input tree first and then from the display's, and list
  folders both have (`/sys`, `/sys/class`, `/sys/devices`,
  `/sys/devices/pci0000:00`, `/sys/bus`, `/sys/dev/char`) as one.
- udev without a daemon: `/run/udev/data/c13:N`, `+input:inputN` and
  `c245:N` hold the properties udev's rules would give (`ID_INPUT_*`,
  `ID_BUS`, `ID_VENDOR_ID`, `ID_SERIAL`, `ID_PATH`, links, `uaccess` and
  `seat` tags), and `/run/udev/control` exists so libudev's monitors listen.
- Hot-plug: every device coming or going is sent on netlink
  (`NETLINK_KOBJECT_UEVENT`) as the kernel's message to group 1 and, with
  udev's properties and a `libudev` header (magic, MurmurHash2 subsystem
  hash, tag bloom filter), as udev's to group 2, from port 0 with the
  system's credentials. libudev's monitor (SDL, Firefox) and enumerator
  work unchanged. inotify on `/dev/input` reports `IN_CREATE` then
  `IN_ATTRIB` for new nodes and `IN_DELETE` for removed ones (SDL's path
  without udev, `SDL_JOYSTICK_DISABLE_UDEV=1`).

## The interfaces

- **evdev**: `struct input_event` reads framed by `SYN_REPORT` (a reader
  sees a report once it is complete); a reader 256 events behind loses
  them and gets `SYN_DROPPED` and the newest event; `O_NONBLOCK`, `poll`,
  `select`, `epoll`; `ENODEV` and `POLLHUP` once unplugged. ioctls:
  `EVIOCGVERSION`, `EVIOCGID`, `EVIOCGNAME`, `EVIOCGPHYS`, `EVIOCGUNIQ`,
  `EVIOCGPROP`, `EVIOCGBIT` (every type), `EVIOCGABS`/`EVIOCSABS`,
  `EVIOCGMTSLOTS`, `EVIOCGKEY`, `EVIOCGLED`, `EVIOCGSND`, `EVIOCGSW`,
  `EVIOCGRAB`, `EVIOCREVOKE`, `EVIOCSCLOCKID` (realtime, monotonic,
  boottime), `EVIOCGREP`/`EVIOCSREP`, `EVIOCGEFFECTS`, `EVIOCSFF`,
  `EVIOCRMFF`; writes of `EV_FF` (play, gain) and `EV_LED` (Caps Lock
  lights every keyboard without changing the lock). Key repeat (value 2)
  comes from the kernel for keyboards. Not there: `EVIOCGKEYCODE` /
  `EVIOCSKEYCODE` (no scancode tables; `EINVAL`) and `EVIOCGMASK` /
  `EVIOCSMASK`.
- **joydev**: `struct js_event`, the opening `JS_EVENT_INIT` events,
  `JSIOCGVERSION`, `JSIOCGAXES`, `JSIOCGBUTTONS`, `JSIOCGNAME`,
  `JSIOCS/GCORR`, `JSIOCS/GAXMAP`, `JSIOCS/GBTNMAP`.
- **hidraw**: one input report per read (with its id first), output
  reports by `write` (the interrupt OUT endpoint, else `SET_REPORT`),
  `HIDIOCGRDESCSIZE`, `HIDIOCGRDESC`, `HIDIOCGRAWINFO`, `HIDIOCGRAWNAME`,
  `HIDIOCGRAWPHYS`, `HIDIOCGRAWUNIQ`, `HIDIOCS/GFEATURE`, `HIDIOCS/GINPUT`,
  `HIDIOCS/GOUTPUT` (`GET_REPORT`/`SET_REPORT` control transfers). Every
  USB HID interface has one, including controllers (SDL's and Steam's
  HIDAPI drivers use these for PlayStation and Switch controllers). Xbox
  controllers aren't HID and have none (as on Linux). I²C HID devices have
  none yet.
- **uinput** (version 5): `UI_SET_*BIT`, `UI_DEV_SETUP`, `UI_ABS_SETUP`,
  `UI_SET_PHYS`, the old `struct uinput_user_dev`, `UI_DEV_CREATE`,
  `UI_DEV_DESTROY`, `UI_GET_SYSNAME`, `UI_GET_VERSION`, and force feedback
  back to the maker (`EV_UINPUT` requests, `UI_BEGIN/END_FF_UPLOAD`,
  `UI_BEGIN/END_FF_ERASE`, `EV_FF` plays, 30 s for an answer). A device
  with gamepad buttons and `ABS_X` becomes a WaveOS controller too
  (`drivers::gamepad`, without rumble from WaveOS programs); keys and
  relative or absolute pointer motion go into WaveOS's input. Touchpads
  made this way are Linux-only.
- **inotify**: `inotify_init`, `inotify_init1`, `inotify_add_watch`
  (`IN_ONLYDIR`, `IN_DONT_FOLLOW`, `IN_MASK_ADD`, `IN_MASK_CREATE`,
  `IN_ONESHOT`), `inotify_rm_watch` (`IN_IGNORED`), `FIONREAD`. Device
  nodes, and files and folders created, deleted and renamed on WaveOS's
  file systems are reported; changes to a file's contents (`IN_MODIFY`,
  `IN_CLOSE_WRITE`) and opens aren't yet.

## Tests

`cargo xtask test` runs `linux-tests/inputtest.c` ("Linux programs: input
devices …"). The program prints `@command` lines and the kernel test
(`tests.rs`, `InputTestUser`) answers on its standard input, playing the
user: it connects, presses and disconnects the keyboard-played controller
(the same `play::pad::Virtual` Super+G uses), types on the PS/2 keyboard,
moves the focus, adds a touchpad and a raw HID controller made in
software, and reports what WaveOS's own input and the controller's motors
received. The program checks enumeration and sysfs, the udev records,
every ioctl above, event framing and timestamps, focus, grabs, overflow,
rumble to the motors and back to a uinput device's maker, hot-plug over
netlink (both groups) and inotify, and uinput devices reaching WaveOS.
`drivers::evdev::self_test` checks the queue's overflow rule, the joystick
correction and the code tables.

### SDL, unmodified

`linux-programs/sdlprobe` is a small program linked against Debian's
`libSDL2-2.0.so.0` (SDL 2.26.5, in the Linux packages; built by xtask like
Voyage and put on the disk at `/usr/lib/waveos-tests/sdlprobe`). It
initialises SDL's game controller subsystem, waits a while for controllers
to be plugged in, and reports each joystick: name, GUID, whether SDL calls
it a game controller and the mapping it chose. On a disk with the Linux
packages (`cargo xtask run --linux-root`), in Terminal:

```sh
sh -c "/usr/lib/waveos-tests/sdlprobe 20 2>/dev/kmsg"      # then Super+G
sh -c "SDL_JOYSTICK_DISABLE_UDEV=1 /usr/lib/waveos-tests/sdlprobe 20 2>/dev/kmsg"
```

Both ways (SDL's libudev enumeration and monitor; its `/dev/input` scan
and inotify), with the keyboard-played controller connected after SDL
started, SDL reports one controller-added event and:

```
joystick 0 "Xbox 360 Controller" guid 030003f05e0400008e02000014010000: game controller
  name "X360 Controller", mapping 030003f05e0400008e02000014010000,X360 Controller,a:b0,b:b1,back:b6,
  dpdown:h0.4,dpleft:h0.8,dpright:h0.2,dpup:h0.1,guide:b8,leftshoulder:b4,leftstick:b9,lefttrigger:a2,
  leftx:a0,lefty:a1,rightshoulder:b5,rightstick:b10,righttrigger:a5,rightx:a3,righty:a4,start:b7,x:b2,y:b3,
  opened; rumble yes
```

— SDL's built-in Xbox 360 mapping, matched by GUID. What libudev needed
besides the files: `O_PATH | O_NOFOLLOW` opens of symbolic links with
`readlinkat(fd, "")` (systemd's path walking), `statfs` saying sysfs for
`/sys`, and folder times in `/dev/input` that change with the devices
(SDL rescans when they do).
