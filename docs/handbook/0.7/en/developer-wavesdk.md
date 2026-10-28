# WaveSDK
The WaveSDK is everything you need to make apps for WaveOS. It includes the WaveUI framework, CoreKit for files, processes and networking, Studio, the `wavesdk` command and the GINA package format. The SDK used to be called Constellation. The old `constellation` command still works, and packages that name SDK 0.6 to 0.8 still install.

## What is in the SDK
| Part | What it does |
|---|---|
| WaveUI | Declarative interfaces in the WaveOS look. See [WaveUI](developer-waveui.md). |
| CoreKit | Files, processes, threads, preferences, the clipboard, notifications, networking and IPC |
| Studio | Create projects, edit code, lay out a window, and export or install packages |
| `wavesdk` | The same operations from Terminal |
| GINA | The package format apps are installed from. See [GINA package format](developer-packages.md). |

The SDK sources are in `/System/Developer/SDK`. The WaveUI Gallery example, which shows every control, is in `/System/Developer/Examples/WaveUIGallery.gina`.

## Create a project
1. Open Studio and choose **New**, or run `wavesdk new /Documents/Projects/MyApp dev.example.myapp "My App"`.
2. Edit the app in `src/main.rs`. It starts as a small WaveUI app with a button.
3. Lay out simple screens in Design. Studio saves them to `layouts/main.ui` and writes the WaveUI code to `src/generated_ui.rs`. Your `src/main.rs` is never overwritten.
4. Save the project.

A project contains:
```text
MyApp/
  manifest.toml        the app's identity, version and SDK
  Cargo.toml           depends on corekit and waveui from the SDK
  src/main.rs          your code
  src/generated_ui.rs  written from layouts/main.ui
  layouts/main.ui      one component per line: heading, label, button, toggle
```

## Commands
| Command | Result |
|---|---|
| `wavesdk doctor` | Lists the native tools that are missing |
| `wavesdk new PROJECT APP_ID NAME` | Creates a project |
| `wavesdk generate PROJECT` | Writes `src/generated_ui.rs` from the layout |
| `wavesdk build PROJECT` and `wavesdk run PROJECT` | Compile and run, once native tools are present |
| `wavesdk package PROJECT` | Exports a `.gina` from `build/app.elf` |
| `wavesdk inspect PACKAGE` | Shows a package's name, ID, developer, version and file count |
| `wavesdk install PACKAGE` | Installs or updates an app |
| `wavesdk list` | Lists installed apps |
| `wavesdk launch APP_ID` | Starts an installed app |
| `wavesdk uninstall APP_ID` | Removes an app but keeps its data |

## Build on a host
Rust, Cargo and LLVM do not yet run inside WaveOS. Build and Run report the missing tools; they never pretend to succeed. Until the toolchain is ported, build apps on a host with the repository:

1. Add your app to the userland workspace, with `waveui` and `corekit` as dependencies.
2. Run `cargo xtask build` to compile the system and your app.
3. Package the executable with `cargo xtask package manifest.toml app.elf MyApp.gina`.
4. Open the package in Files, or run `wavesdk install MyApp.gina`.

## Test your app
Before you ship, check that your app:
- Resizes cleanly, down to its smallest size.
- Works with the keyboard alone, with Tab, Space, Return and Escape.
- Looks right in light and dark appearance, and with a different accent colour.
- Respects Reduce Motion and larger text.
- Asks before closing a window with unsaved work.
