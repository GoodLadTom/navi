# Makes the test clips for the live voice-command measurement (src-tauri/src/spoken.rs) with the voices
# built into Windows: 16 kHz, 16-bit, mono WAVs plus a clips.json manifest.
#
#   powershell -ExecutionPolicy Bypass -File scripts\make_voice_clips.ps1 -Out <folder>
#
# Then, from src-tauri:
#   $env:NAVI_VOICE_CLIPS = '<folder>'
#   cargo test --lib spoken::tests::live_detection_on_the_running_engine -- --ignored --nocapture
#
# Every clip is made four times: Hazel (en-GB) and Zira (en-US), at Rate 0 and 2. "Navi" is written
# "Nahvee" in what is spoken (SAPI otherwise says "navvy"); the manifest keeps "Navi".
# [600] in a text is a pause of 600 ms. Each clip starts with 600 ms of silence and ends with 1.5 s.
param(
  [string]$Out = (Join-Path $PSScriptRoot '..\target\voice-clips')
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech

$voices = @(
  @{ name = 'Microsoft Hazel Desktop'; code = 'hazel'; culture = 'en-GB' },
  @{ name = 'Microsoft Zira Desktop'; code = 'zira'; culture = 'en-US' }
)
$rates = @(0, 2)
$heyToo = @('Navi', 'Hey Navi', 'Hey')

# n, kind, text, expect, wakeOnly (clip number of the wake-alone clip), wake (custom wake words)
$clips = @(
  @{ n = 1; kind = 'idle'; text = 'Navi.'; expect = 'wake' },
  @{ n = 2; kind = 'idle'; text = 'Hey Navi.'; expect = 'wake' },
  @{ n = 3; kind = 'idle'; text = 'Navi, watch my screen.'; expect = 'command:watch_on' },
  @{ n = 4; kind = 'idle'; text = 'Navi watch my screen'; expect = 'command:watch_on' },
  @{ n = 5; kind = 'idle'; text = 'Hey Navi, take notes.'; expect = 'command:notes_on' },
  @{ n = 6; kind = 'idle'; text = 'Navi, stop taking notes.'; expect = 'command:notes_off' },
  @{ n = 7; kind = 'idle'; text = 'Navi, stop watching.'; expect = 'command:watch_off' },
  @{ n = 8; kind = 'idle'; text = 'Navi, scratch that.'; expect = 'command:scratch_that' },
  @{ n = 9; kind = 'idle'; text = 'Navi, email Sam about the meeting on Friday.'; expect = 'dictation'; wakeOnly = 1 },
  @{ n = 10; kind = 'idle'; text = 'Hey Navi, make that shorter.'; expect = 'dictation'; wakeOnly = 2 },
  @{ n = 11; kind = 'idle'; text = 'Navy blue is my favourite colour.'; expect = 'ignore' },
  @{ n = 12; kind = 'idle'; text = 'Navi is our new dictation app.'; expect = 'ignore' },
  @{ n = 13; kind = 'idle'; text = 'Navigate to the settings page.'; expect = 'ignore' },
  @{ n = 14; kind = 'idle'; text = 'I told Navi to take notes.'; expect = 'ignore' },
  @{ n = 15; kind = 'idle'; text = 'Take notes on the meeting tomorrow.'; expect = 'ignore' },
  @{ n = 16; kind = 'idle'; text = 'Watch my screen for a second.'; expect = 'ignore' },
  @{ n = 17; kind = 'idle'; text = 'Hey there, how are you doing?'; expect = 'ignore' },
  @{ n = 18; kind = 'idle'; text = 'Have a lovely weekend.'; expect = 'ignore' },
  @{ n = 19; kind = 'call'; text = 'Navi, stop taking notes.'; expect = 'command:notes_off' },
  @{ n = 20; kind = 'call'; text = 'Navi, draft the follow-up email to Sam.'; expect = 'ignore' },
  @{ n = 21; kind = 'call'; text = 'Navi.'; expect = 'ignore' },
  @{ n = 22; kind = 'take'; text = 'Thanks for the update, I will look at it tonight. [900] Don''t worry.'; expect = 'cancel' },
  @{ n = 23; kind = 'take'; text = 'That is fine by me. [900] That''s all.'; expect = 'finish' },
  @{ n = 24; kind = 'take'; text = 'Don''t worry about the deadline, I will sort it out.'; expect = 'none' },
  @{ n = 25; kind = 'take'; text = 'That''s all I wanted to say about the budget.'; expect = 'none' },
  @{ n = 26; kind = 'take'; text = 'Don''t worry [350] about the deadline.'; expect = 'none' },
  @{ n = 27; kind = 'take'; text = 'Navi, don''t worry.'; expect = 'cancel' },
  @{ n = 28; kind = 'wake-take'; text = 'Navi. [1000] Watch my screen.'; expect = 'command:watch_on' },
  @{ n = 29; kind = 'wake-take'; text = 'Navi. [1000] Don''t worry.'; expect = 'cancel' },
  @{ n = 30; kind = 'wake-take'; text = 'Navi. [800] Email Sam about Friday please.'; expect = 'none' },
  # Extra cases: commands after a longer pause, and more ordinary speech that must not trigger.
  @{ n = 31; kind = 'idle'; text = 'Navi, write to Sam about Friday.'; expect = 'dictation'; wakeOnly = 1 },
  @{ n = 32; kind = 'idle'; text = 'Hey.'; expect = 'wake'; wake = $heyToo },
  @{ n = 33; kind = 'idle'; text = 'Hey, take notes.'; expect = 'command:notes_on'; wake = $heyToo },
  @{ n = 34; kind = 'idle'; text = 'Hello, how are you?'; expect = 'ignore'; wake = $heyToo },
  @{ n = 35; kind = 'take'; text = 'Thanks for the update. [900] Don''t worry about the deadline, it''s fine.'; expect = 'none' },
  @{ n = 36; kind = 'idle'; text = 'Navi is our new app.'; expect = 'ignore' },
  @{ n = 37; kind = 'idle'; text = 'Stop watching.'; expect = 'ignore' },
  @{ n = 38; kind = 'wake-take'; text = 'Navi. [1000] Stop watching.'; expect = 'command:watch_off' },
  @{ n = 39; kind = 'call'; text = 'Navi, stop watching.'; expect = 'command:watch_off' },
  @{ n = 40; kind = 'take'; text = 'Don''t worry about the deadline, it''s fine.'; expect = 'none' },
  # SAPI adds its own pause after a full stop (a [1000] after "Navi." comes out near 1.9 s); these
  # two keep the gap short, so the command starts while the idle listener is still deciding.
  @{ n = 41; kind = 'wake-take'; text = 'Navi, [500] watch my screen.'; expect = 'command:watch_on' },
  @{ n = 42; kind = 'wake-take'; text = 'Navi, [500] don''t worry.'; expect = 'cancel' }
)

New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono)

function ClipName($n, $v, $r) { '{0:D2}-{1}-r{2}.wav' -f $n, $v.code, $r }

$manifest = New-Object System.Collections.ArrayList
foreach ($c in $clips) {
  foreach ($v in $voices) {
    foreach ($r in $rates) {
      $file = ClipName $c.n $v $r
      $culture = [System.Globalization.CultureInfo]::GetCultureInfo($v.culture)
      $pb = New-Object System.Speech.Synthesis.PromptBuilder($culture)
      $pb.StartVoice($v.name)
      $pb.AppendBreak([TimeSpan]::FromMilliseconds(600))
      foreach ($part in [regex]::Split($c.text, '\s*(\[\d+\])\s*')) {
        if ($part -match '^\[(\d+)\]$') {
          $pb.AppendBreak([TimeSpan]::FromMilliseconds([int]$Matches[1]))
        } elseif ($part.Trim()) {
          $pb.AppendText(($part -creplace '\bNavi\b', 'Nahvee'))
        }
      }
      $pb.AppendBreak([TimeSpan]::FromMilliseconds(1500))
      $pb.EndVoice()
      $s = New-Object System.Speech.Synthesis.SpeechSynthesizer
      $s.SelectVoice($v.name)
      $s.Rate = $r
      $s.SetOutputToWaveFile((Join-Path $Out $file), $fmt)
      $s.Speak($pb)
      $s.SetOutputToNull()
      $s.Dispose()
      $entry = [ordered]@{ file = $file; text = $c.text; voice = $v.code; rate = $r; kind = $c.kind; expect = $c.expect }
      if ($c.wakeOnly) { $entry.wakeOnly = (ClipName $c.wakeOnly $v $r) }
      if ($c.wake) { $entry.wake = @($c.wake) }
      [void]$manifest.Add($entry)
    }
  }
}
$json = ConvertTo-Json -InputObject @($manifest) -Depth 4
[System.IO.File]::WriteAllText((Join-Path $Out 'clips.json'), $json, (New-Object System.Text.UTF8Encoding($false)))
"made $($manifest.Count) clips in $Out"
