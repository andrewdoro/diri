# Signed-install wake test (run by the user)

These steps deliberately sleep/wake your Mac. Save work first; use AC power,
leave the lid open, and finish unrelated agent runs. Do not run the helper by
hand, use sudo, or load its plist with launchctl.

1. Install the signed review build normally. Open Diri → Schedules. Switch on
   **Allow diri to wake the Mac**. Follow the link to System Settings → General
   → Login Items & Extensions and approve Diri's background helper. Return to
   Schedules; it should show enabled. Leave Diri running and logged in.
2. Create a **Once** schedule five minutes ahead, with **Wake Mac** enabled,
   using an already authenticated agent and an empty test folder. Prompt:
   “Create wake-test.txt in this folder containing the current date/time, then
   finish.” Do not grant the agent extra permissions. Record the schedule ID
   and due time. Wait until `schedule.list` has no `wakeHelperError`.
3. In Terminal, run `pmset -g sched`. Expect an event owned by
   `com.dirijor.diri.wake.<your uid>` about **two minutes before** the due time.
   Choose Apple menu → Sleep. Do not touch the keyboard, mouse or trackpad.
4. Expect a wake around the alarm, a new session near the due time, and exactly
   one file/run. A dark wake may promote to full wake; the display can light up
   but stays locked. Diri should release its assertion after at least two
   continuously idle minutes, then sleep if the console is still yours and no
   input occurred since the alarm. Unknown kernel wake reasons fail closed:
   the run still works, but no automatic sleep or indigo mark is authorized.
5. Wake the Mac yourself and inspect:

   ```sh
   pmset -g sched
   pmset -g log | grep -E 'Wake |DarkWake|Sleep |com.dirijor.diri.wake'
   printf '%s\n' '{"id":1,"method":"schedule.list","params":{}}' |
     nc -w 2 -U "$HOME/Library/Application Support/Dirijor/daemon.sock"
   printf '%s\n' '{"id":2,"method":"session.list","params":{}}' |
     nc -w 2 -U "$HOME/Library/Application Support/Dirijor/daemon.sock"
   grep 'diri-scheduler:' "$HOME/Library/Application Support/Dirijor/logs/dirijord.log" | tail -20
   ```

   Use the configured Engine socket/log paths if this install overrides them.
   The Once schedule should be disabled with one `onTime` (or bounded `late`)
   run and a `sessionId`. That session should retain `scheduledRun`, a clock,
   and `wokeMac: true`/indigo only for an acknowledged RTC wake. The log should
   say `sleep after run: slept=true`; refusals show `slept=false`, and helper
   failures include `wake sync failed` or `sleep after run failed`.
6. Repeat with a five-minute schedule, but touch input **after the alarm and
   before the due time**. It must run without putting the Mac back to sleep
   (`slept=false`). Also run a schedule while already awake: clock, no indigo,
   and no sleep-after request. If testing overlap, keep a second agent working;
   Diri must wait for it too. An Engine restart during a run must restore the
   awake hold but must not restore permission to force sleep.
7. Cleanup: delete the test schedules, wait for synchronization, and verify
   `pmset -g sched` contains no remaining Diri test alarms (other apps' events
   must remain). Close only the test sessions and remove the test folder. If
   desired, turn off the wake helper **after** the alarm list is clear. Record
   hardware model, macOS version, wake reason, timestamps, and any discrepancy.
