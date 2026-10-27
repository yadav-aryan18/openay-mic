OpenAY Mic — Windows client (openay-server.exe)
===============================================

The same openay-server binary that runs on Linux, cross-compiled for
Windows (x86_64-pc-windows-msvc). It receives the phone's 48 kHz audio
stream and plays it through a Windows audio output device.

Requirements
------------
- Windows 10 or newer (x86_64). No admin rights needed to run the exe.
- A sound output device. To expose the stream to other apps as a
  microphone, install a virtual audio cable (e.g. VB-Cable, free from
  vb-audio.com) and select it as the output device — apps then pick
  "CABLE Output" as their input. To simply monitor the audio, any
  speakers/headset works.
- The phone app (OpenAY Mic on Android) and this receiver.

Firewall (one-time; requires an elevated (Administrator) Command Prompt;
ports TCP+UDP 41700 — running the exe itself needs no admin)
------------------------------------------------------------------------
netsh advfirewall firewall add rule name="OpenAY Mic" dir=in action=allow ^
  protocol=TCP localport=41700
netsh advfirewall firewall add rule name="OpenAY Mic UDP" dir=in action=allow ^
  protocol=UDP localport=41700

(Remove later with: netsh advfirewall firewall delete rule name="OpenAY Mic"
 and name="OpenAY Mic UDP".)

Wi-Fi usage
-----------
1. Find your PC's LAN IP (ipconfig) — e.g. 192.168.1.10.
2. Start the receiver:
       openay-server.exe --transport udp --output
   (--output renders the decoded stream to the default output device.)
3. In the phone app choose Wi-Fi transport and connect to
   192.168.1.10:41700 (your PC IP, port 41700). Both phone and PC must be
   on the same network; keep the phone off mobile data.

USB usage (lower latency, more reliable)
----------------------------------------
1. Enable USB debugging on the phone, connect the cable.
2. Forward the port with adb (from Android SDK platform-tools):
       adb.exe forward tcp:41700 tcp:41700
3. Start the receiver:
       openay-server.exe --transport tcp --output
4. In the phone app choose TCP transport and connect to 127.0.0.1:41700.

Audio output options
--------------------
  openay-server.exe --list-devices          list output devices
  openay-server.exe --output-device "CABLE Input" --output
                                            render into the virtual cable
  openay-server.exe --output                default output device
--output, --output-device and --list-devices all require the audio-output
feature (enabled in the distributed exe); a build without it still
receives on the network but has no playback.

The GUI console (openay-gui.exe)
--------------------------------
The dist also ships openay-gui.exe — the same studio console as on Linux
(The Chain, VU ladder, ON AIR toggle, settings slide-over, persisted
config), rendering the phone's audio with WASAPI instead of PipeWire:

    .\openay-gui.exe

It starts stopped (cold); press ON AIR, pick transport/port like on Linux.
Audio renders on the system DEFAULT output device — to feed other apps,
install a virtual audio cable, set "CABLE Input" as the default playback
device in Windows Sound settings, and select "CABLE Output" in the consumer
app (or edit output_device in %APPDATA%\openay-mic\config.toml).
Differences from Linux, on purpose: no system tray (window close quits —
same contract), autostart uses the Startup folder, audio routing happens
in Windows Sound settings instead of WirePlumber.

Collecting logs for bug reports
-------------------------------
Use the wrapper instead of the bare exe and every session is captured
automatically (console + a timestamped file under
%USERPROFILE%\OpenAY-logs\):

    .\openay-log.ps1 --transport udp --output
    .\openay-log.ps1 --transport tcp --output --output-device "CABLE Input"

(If script execution is blocked: powershell -ExecutionPolicy Bypass
-File .\openay-log.ps1 ...) The wrapper also snapshots --list-devices
into the log directory. If something misbehaves, zip that OpenAY-logs
folder and attach it.

Phone side (optional, needs USB debugging): while testing, capture the
app's own log in a second terminal:

    adb logcat -s MicCaptureService:* openaymic:* > phone.logcat

Common flags
------------
  --transport udp|tcp     transport to receive on (default udp)
  --port 41700            port to listen on
  --codec auto|pcm|opus   which payloads to accept (default auto)
  --target-ms 10          prebuffer target, 5..20 ms
  --capacity-ms 100       jitter buffer capacity

Rebuilding from source
----------------------
On a Linux host:  scripts/build_windows.sh
(cross-compiles via cargo-xwin in user space and stages dist/openay-windows/)
