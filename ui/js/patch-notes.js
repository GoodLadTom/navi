// What changed in each version, newest first, shown at the bottom of Settings ("What's new").
// scripts/release.py refuses to build a release whose version isn't here, so add the new entry
// first. Plain text only; keep each line to one change a person would notice.
export const PATCH_NOTES = [
  {
    version: '0.1.8',
    date: '26 November 2026',
    changes: [
      'What’s new: this list. Every update now says what changed, here at the bottom of Settings.',
      'On a Mac, your shortcut no longer stops working until you restart Navi. macOS hides keys while a password box or the lock screen is up, so Navi could miss a key being let go, think it was still held and ignore your shortcut. Navi now checks with macOS.',
      'On a Mac, if the shortcut listener stops hearing keys, Navi notices within seconds, starts a fresh one and notes it in the log.',
      'On a Mac, when another program leaves Secure Input switched on (it can stay on after you log in), Navi types as normal instead of treating every box as a password field. Your words stay on the clipboard too, and Navi says so once. Real password fields still get the clipboard.',
    ],
  },
  {
    version: '0.1.7',
    date: '25 October 2026',
    changes: [
      'Updates on Windows no longer stop with “Failed to kill Navi”: the installer now waits for Navi to finish closing.',
    ],
  },
  {
    version: '0.1.6',
    date: '25 October 2026',
    changes: [
      'Action keys, in Settings > Shortcuts. Select text, press your Ask AI key and say what you want: a change replaces the selection, and a question gets an answer in the middle of the screen.',
      'One-press rewrites with your own instructions, and a key for every voice command.',
      'The bubble says when your AI is working, with a clock.',
    ],
  },
  {
    version: '0.1.5',
    date: '24 October 2026',
    changes: [
      'Select text and say “Navi, scratch that” to delete it. Without a selection it still removes the last thing Navi typed.',
    ],
  },
  {
    version: '0.1.4',
    date: '23 October 2026',
    changes: [
      'Call notes no longer use “Them” as if it were a name. They use the person’s name when it’s said on the call, or “the other person” and “they”.',
    ],
  },
  {
    version: '0.1.3',
    date: '23 October 2026',
    changes: [
      'Navi now starts in Clean mode. Auto, which picks the mode from the app you’re in, is one click away in the tray or on the Modes page.',
    ],
  },
  {
    version: '0.1.2',
    date: '23 October 2026',
    changes: [
      'Hands-free takes, rambles and “Navi” takes finish with a tap of your shortcut, and Enter types as normal while Navi listens. Settings > Shortcuts can let Enter finish them again.',
      'Holding the shortcut to talk works on a Mac.',
      'A Mac copy can update itself from Settings > About.',
    ],
  },
  {
    version: '0.1.1',
    date: '21 October 2026',
    changes: [
      'The first public release: free, open-source dictation for Windows and Mac. Hold a key, talk, let go, and your words appear wherever you were typing. Speech is recognised on your own computer.',
    ],
  },
];
