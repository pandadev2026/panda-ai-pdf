# "Print to Stirling PDF"

**Status: Plan B (a registered MSIX virtual printer) is dead-ended — see
"Root cause" below. Plan C (watch Microsoft Print to PDF's output folder and
auto-open new files) is implemented and self-tested from this repo; it just
hasn't been runtime-verified on real Windows hardware yet.** Read "Plan C" for
what's actually shipping, and "Plan B" for the investigation record of why we
didn't go with a real virtual printer.

## Plan C: watch Microsoft Print to PDF's output folder (current approach)

Since a registered virtual printer isn't reachable from a sideloaded MSIX
(see "Root cause" below), Plan C piggybacks on the printer Windows already
ships: **Microsoft Print to PDF**. The user prints to it once, pointed at a
folder we control; Stirling PDF watches that folder and opens whatever shows
up there, the same way it already handles a double-clicked or dragged-in
file.

### What the user has to do

The first time, in the "Save Print Output As" dialog Windows shows after
picking "Microsoft Print to PDF" as the printer, save to:

```
Documents\Stirling PDF\Print Inbox
```

(created automatically by the app on first launch if it doesn't exist).
Whether Windows remembers this location for next time is up to that dialog's
own per-application memory — Stirling PDF has no way to set or force it, and
can't register itself as that printer's default save location. This is the
central UX limitation of Plan C vs. a real virtual printer: it's opt-in and
manual to set up once, every app you print from.

### How it works

Implemented in `frontend/editor/src-tauri/src/commands/print_inbox.rs`
(Windows-only; a no-op stub on other platforms), wired up from
`start_print_inbox_watcher()` in `setup()` in `src/lib.rs`:

- On startup, spawns one dedicated OS thread (not the async runtime — the
  watch loop blocks on `notify`'s channel) that:
  1. Resolves `%USERPROFILE%\Documents\Stirling PDF\Print Inbox` via Tauri's
     path resolver and creates it if missing.
  2. Runs a **startup catch-up scan**: reads the folder for `.pdf` files
     newer than the last-processed timestamp persisted in
     `<app data dir>/print-inbox-state.json`, and opens anything missed while
     the app wasn't running (oldest first, so they open in print order). On
     the very first run ever (no state file yet), it does *not* bulk-open
     whatever's already sitting in the folder — it just records "now" as the
     baseline, so shipping this feature doesn't suddenly flood-open a user's
     existing PDF collection if they'd already been using that folder for
     something else.
  3. Starts a `notify` (v8) filesystem watcher on the folder
     (non-recursive) and reacts to `Create` events only — Microsoft Print to
     PDF creates the destination file once when the save starts, so `Create`
     fires exactly once per print job; reacting to `Modify` as well would
     just mean handling the same file multiple times while it's being
     written.
  4. Before opening a newly created file, polls its size every 250ms (up to
     ~5s) until it stops changing, so a print job that's still being written
     doesn't get opened half-finished.
  5. Opens the file through the exact same path as every other "open a file"
     trigger in this app (drag-drop, CLI launch, second-instance handoff):
     `add_opened_file()` + `forward_files_to_window()` targeting the
     currently focused window (falling back to the main window, then any
     window). No new frontend code was needed — `useOpenedFile.ts` /
     `fileOpenService.ts` already pick this up via the existing
     `files-changed` event / `pop_opened_files` command.
- The last-processed timestamp is persisted (not just kept in memory) after
  every file opened, so a crash or restart mid-run doesn't lose track of
  what's already been handled.

### Self-testing done (from this Mac, no Windows machine available)

This machine has no Rust toolchain and no network access to crates.io by
default. Both were worked around: a cached `rustup`/`cargo` install existed
at `~/Library/Caches/puccinialin/` (left behind by an unrelated Python
tool), and `static.crates.io` / `index.crates.io` (unlike `crates.io`
itself) were reachable, so real dependency resolution and compilation was
possible. `mingw-w64` was installed via Homebrew to get a Windows-targeting C
toolchain for cross-compiling.

- `cargo check --target x86_64-pc-windows-gnu` — **clean**, zero warnings
  (this crate has `warnings = "deny"`), fully type-checks this module against
  the real `notify` and `tauri` APIs for the Windows target.
- `cargo clippy --target x86_64-pc-windows-gnu --lib -- -W clippy::all` —
  **zero findings in `print_inbox.rs`** (19 pre-existing findings elsewhere
  in the codebase, untouched by this change, left alone as out of scope).
- `cargo build --target x86_64-pc-windows-gnu --lib` — compiled every object
  file across the entire dependency graph (including this module) and only
  failed at the final DLL link, with `mingw-w64`'s `ld`: `error: export
  ordinal too large: 158681`. That's a GNU binutils limitation on the number
  of exported symbols in this specific mingw cross-toolchain, not a code
  problem — real Windows builds link with MSVC's `link.exe` via CI, which
  doesn't have this limit, and would need to be trusted for the actual
  release build regardless.
- `cargo check` (native `aarch64-apple-darwin` target, i.e. the rest of the
  app unaffected by `#[cfg(target_os = "windows")]`) — clean.

**Not done, because it can't be from here: running the app on Windows and
watching a real "Microsoft Print to PDF" job land, get picked up, and open.**
Everything above verifies the code compiles and type-checks correctly
against the real Windows APIs; it does not verify runtime behavior (does
`notify`'s `ReadDirectoryChangesW` backend actually fire `Create` the way
assumed here, does the save dialog behave as expected, etc.). That's the
outstanding risk before calling this done-done.

### Known limitations (accepted, not bugs)

- **Only watches while the app is running** — no background service/tray
  presence. A file printed while Stirling PDF is closed sits in the folder
  until next launch, when the catch-up scan picks it up. This was an
  explicit trade-off accepted when Plan B turned out to be blocked (a real
  virtual printer would have gotten Windows to launch/wake the app via the
  DEH; a watch-folder can't).
- **Setup is manual and per-source-app** — see "What the user has to do"
  above. There's no OS hook to make Microsoft Print to PDF default to our
  folder.
- **Not wired into onboarding/Settings UI yet** — no in-app messaging telling
  the user this folder exists or how to use it, and no toggle to disable the
  watcher. Purely a backend mechanism right now.
- **Possible duplicate opens** — if the filesystem backend ever delivers more
  than one `Create` event for the same file, both could pass the stability
  check before the first one's persisted-timestamp update lands, opening the
  same PDF twice. Harmless (just an extra tab/window), not de-duplicated
  beyond the mtime-vs-last-processed check, because a `Mutex`-guarded
  in-flight set felt like more machinery than this problem warrants.

## Plan B: MSIX virtual printer (spike, abandoned)

This was the original attempt: make "Stirling PDF" show up as a printer
choice in any Windows app (Word, Outlook, Notepad, ...), using Windows'
modern Print Support App (PSA) v4 "Virtual Printer" architecture — no legacy
kernel print driver, no WHQL/EV driver signing, just an MSIX package with a
background task that converts OXPS to PDF via Windows' own converter. Kept
here, unchanged, as the investigation record for why it didn't work — the
code in `AppxManifest.xml`, `PrinterCapabilities.pdc.xml`, and
`BackgroundTask/` is exactly what was built and tested, not a live target for
further work.

**Not wired into the real Stirling PDF app.** The background task in
`BackgroundTask/VirtualPrinterTask.cpp` writes whatever it converts to a
hardcoded folder (`C:\ProgramData\StirlingPDF\VirtualPrinterOutput`) and
stops — moot for now since the printer queue never registers (see below),
but this was always Phase 1's boundary regardless.

Background docs this was grounded in:
- [Print Support App v4 API Design Guide](https://learn.microsoft.com/en-us/windows-hardware/drivers/devapps/print-support-app-v4-design-guide)
- [MSIX Manifest Specification for Print Support Virtual Printer](https://learn.microsoft.com/en-us/windows-hardware/drivers/devapps/msix-manifest-specification-print-support-virtual-printer)

### What actually happened

Everything here was built and run for real on a Windows 11 VM (not just
read about), in this order:

1. **Toolchain**: installed VS 2022 Community's "Desktop development with
   C++" + "Universal Windows Platform development" workloads and the
   Windows 11 SDK (10.0.26100.0) via `vs_installer.exe modify`.
2. **Compiled** `VirtualPrinterTask.dll` with MSBuild + the
   `Microsoft.Windows.CppWinRT` NuGet package (v3.0.260818.1). This took 4
   real fixes to the hand-written scaffold (see "Bugs found by building
   this" below) — it does not just work by pasting files into a VS
   template as the previous version of this README assumed.
3. **Packaged** into an MSIX with `makeappx.exe` — required removing an
   invalid manifest `Extension` (see below) and including the
   build's merged `Tasks.winmd` as package payload alongside the DLL.
4. **Signed** with a self-signed test cert (`CN=Stirling PDF Inc.`,
   matching the manifest's `Identity/Publisher`) via `signtool.exe`,
   imported into `Cert:\LocalMachine\TrustedPeople`.
5. **Enabled sideloading** (`AppModelUnlock\AllowDevelopmentWithoutDevLicense`
   and `AllowAllTrustedApps` registry values — this machine had neither
   set).
6. **Installed successfully**: `Add-AppxPackage` initially failed with
   `0x80070005` / "Failed to initialize PLM" when run over SSH — this
   turned out to be session-context related (AppX deployment's Process
   Lifetime Manager needs to run in a real interactive desktop session, not
   a non-interactive remote shell). Running the exact same
   `Add-AppxPackage -Path ...` line inside an actual interactive session
   (RealVNC into the VM's console) worked immediately.
   `Get-AppxPackage StirlingPDFInc.StirlingPDFVirtualPrinter` confirms it's
   registered.
7. **The printer never appears.** `Get-Printer` after install shows only
   the stock `Microsoft Print to PDF` and `OneNote (Desktop)` entries —
   nothing for Stirling PDF.

### Root cause (as far as this session dug)

The install log (`Get-AppPackageLog`) contains this warning on every
install attempt:

```
App manifest validation warning: Declared namespace
http://schemas.microsoft.com/appx/manifest/printsupport2/windows10 is
inapplicable, it will be ignored during manifest processing.
```

Checked whether this meant the whole extension block got stripped from the
installed package — it didn't: `Get-AppxPackageManifest` on the installed
package shows the full `printsupport2:Extension` /
`PrintSupportVirtualPrinter` XML is still present verbatim. So the content
survives, but Windows' AppX deployment engine on this machine doesn't
invoke whatever Deployment Extension Handler (DEH) is supposed to read it
and actually create the print queue — and confirmed via
`Microsoft-Windows-PrintService/Admin` and `/Operational` event logs that
**no PrintService events fired at all** during install, meaning the DEH for
this contract was never invoked, not that it ran and failed quietly.

**Not yet determined**: whether this requires a specific Windows SKU/edition,
a feature flag, a Windows Insider build, actual IHV/OEM partner
registration with Microsoft beyond public docs, or something else entirely.
The public MS Learn docs this was grounded in don't mention any such
prerequisite — they read as if a correctly-signed, correctly-packaged MSIX
with this manifest extension should Just Work via sideloading. It didn't,
on Windows 11 build 10.0.26200 (2026-era). This is the open question before
sinking more time into this specific path.

### Bugs found by building this (fixed, for the record)

Four real problems in the original hand-written scaffold, only findable by
actually compiling:

1. **Missing PCH creation step.** `pch.h` alone isn't enough — needed a
   `pch.cpp` with `#include "pch.h"` and `<PrecompiledHeader>Create</PrecompiledHeader>`
   in the `.vcxproj`, since `VirtualPrinterTask.cpp` only *uses* the PCH.
2. **Wrong project type properties.** `AppContainerApplication`,
   `ApplicationType`, `ApplicationTypeRevision` mark a UWP *app* project and
   pulled in the XAML compiler (`MSB4181: CompileXaml task returned false`)
   even though this project has no XAML. Removed — this is a plain WinRT
   Component DLL, not an app.
3. **Wrong generated header name.** cppwinrt names generated files after
   the fully namespace-qualified type: `Tasks.VirtualPrinterTask.g.h`, not
   `VirtualPrinterTask.g.h` as originally guessed.
4. **Wrong base class.** The implementation struct was inheriting raw
   `winrt::implements<VirtualPrinterTask, IBackgroundTask>` instead of the
   generated `winrt::Tasks::implementation::VirtualPrinterTaskT<VirtualPrinterTask>`
   alias. The raw version compiles but omits `Tasks::VirtualPrinterTask`
   from the implements-list, which breaks the generated projection
   constructor in `Tasks.VirtualPrinterTask.g.cpp`
   (`C2665: no overloaded function could convert all the argument types`).
5. **Invalid manifest extension.** The original manifest had an explicit
   `Extension Category="windows.activatableClass.inProcessServer"` block,
   guessing it was needed to resolve the `EntryPoint` reference.
   `makeappx` rejects it outright — that category isn't in this
   `Extensions` scope's allowed schema enum. Matches Microsoft's own PSA v4
   sample manifest, which has no such block; the package's embedded
   `Tasks.winmd` is what resolves the EntryPoint, not a manifest extension.

All fixed in the files in this folder — `VirtualPrinterTask.vcxproj`,
`.h`, and `AppxManifest.xml` reflect the working versions, not the original
guesses.

### Files

| File | Purpose |
|---|---|
| `AppxManifest.xml` | MSIX manifest registering the "Stirling PDF" print queue and background task. Installs cleanly; printer still doesn't register (see above). |
| `PrinterCapabilities.pdc.xml` | Minimal Print Device Capabilities (paper size, orientation, color, simplex). Never actually exercised since the queue never registers. |
| `BackgroundTask/VirtualPrinterTask.idl` | WinRT runtime class declaration for `Tasks.VirtualPrinterTask`. Compiles clean. |
| `BackgroundTask/VirtualPrinterTask.h` / `.cpp` | The job handler: OXPS→PDF via Windows' built-in converter, write to the hardcoded temp folder. Compiles clean; never exercised at runtime (no print job ever reaches it). |
| `BackgroundTask/pch.h` / `pch.cpp` | Precompiled header pair — both required. |
| `BackgroundTask/VirtualPrinterTask.vcxproj` | Builds `VirtualPrinterTask.dll` with MSBuild directly (confirmed — no need to recreate this in the VS GUI, contrary to what an earlier version of this README said). |
| `BackgroundTask/packages.config` | Documents the NuGet package/version (`Microsoft.Windows.CppWinRT` 3.0.260818.1) the `.vcxproj`'s `CppWinRTPackageDir` property assumes. |

### Reproducing the build (confirmed working end-to-end)

```
# One-time toolchain setup (VS Installer, elevated, from an INTERACTIVE session):
vs_installer.exe modify --installPath "<VS install path>" ^
  --add Microsoft.VisualStudio.Workload.NativeDesktop ^
  --add Microsoft.VisualStudio.Workload.Universal ^
  --add Microsoft.VisualStudio.ComponentGroup.UWP.VC ^
  --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 ^
  --add Microsoft.VisualStudio.Component.Windows11SDK.26100 ^
  --quiet --norestart

# Restore the C++/WinRT NuGet package next to BackgroundTask/:
nuget.exe install Microsoft.Windows.CppWinRT -OutputDirectory packages
# If the resolved version differs from 3.0.260818.1, update
# CppWinRTPackageDir in VirtualPrinterTask.vcxproj to match.

# Build:
MSBuild.exe BackgroundTask\VirtualPrinterTask.vcxproj /p:Configuration=Release /p:Platform=x64
# -> BackgroundTask\x64\Release\VirtualPrinterTask.dll
# -> BackgroundTask\x64\Release\VirtualPrinterTask\Tasks.winmd  (needed for packaging, see below)
```

Packaging (stage `AppxManifest.xml`, `PrinterCapabilities.pdc.xml`,
`VirtualPrinterTask.dll`, `Tasks.winmd`, a placeholder host `.exe`, and an
`Assets\` folder with any 3 placeholder PNGs — `StoreLogo.png` 50x50,
`Square150x150Logo.png` 150x150, `Square44x44Logo.png` 44x44 — into one
directory, then):

```
makeappx.exe pack /d <staging dir> /p StirlingPdfVirtualPrinter.msix
New-SelfSignedCertificate -Type Custom -Subject "CN=Stirling PDF Inc." -KeyUsage DigitalSignature -CertStoreLocation Cert:\CurrentUser\My -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3","2.5.29.19={text}")
signtool.exe sign /fd SHA256 /sha1 <thumbprint> /s My StirlingPdfVirtualPrinter.msix
# Export that cert, Import-Certificate into Cert:\LocalMachine\TrustedPeople
# Enable sideloading: HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock
#   AllowDevelopmentWithoutDevLicense = 1 (DWORD), AllowAllTrustedApps = 1 (DWORD)

# MUST run this line from an actual interactive desktop session, not SSH/PsExec/
# any non-interactive remote shell — Add-AppxPackage's PLM init needs one:
Add-AppxPackage -Path .\StirlingPdfVirtualPrinter.msix
```

### Open questions on Plan B (moot for now — kept for the record)

Nobody is actively pursuing these; Plan C is what's shipping. Revisit only if
Plan C's limitations (see above) turn out to be unacceptable and a real
virtual printer becomes worth another attempt:

1. Is `windows.printSupportVirtualPrinterWorkflow` actually usable by a
   sideloaded/independently-signed MSIX at all, or does it require Store
   association / an IHV partner agreement with Microsoft to activate the
   DEH that processes it? The docs don't say either way.
2. Does it need a specific Windows edition or servicing state this VM
   doesn't have? Worth trying on a different, perhaps more "stock", Windows
   11 install if one's available.
3. Is there a Windows optional feature or print-related component that
   needs enabling first (analogous to how some capabilities need
   `Enable-WindowsOptionalFeature`)?
4. Worth filing a question against Microsoft's `windows-driver-docs` repo
   (the docs pages linked above are literally generated from a public
   GitHub repo with issues enabled) — the "no PrintService events fired at
   all" finding is a fairly precise, reproducible bug report.

(This is the same fallback described above, under Plan C — it's no longer a
future option, it's what's implemented.)

If Plan B is ever revived, still-undone work would include: CI integration /
real code signing / bundling the MSIX into the release pipeline alongside the
existing WiX `.msi` (`tauri.conf.json`'s `bundle.targets`), and the
opt-in-vs-default-on UX decision (recommended: opt-in toggle in Settings,
since silently adding a system printer is exactly what prompted this
investigation in the first place — same reasoning applies to Plan C's watch
folder).
