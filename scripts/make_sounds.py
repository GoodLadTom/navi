"""Navi's sound sets: soft, low, calm cues you can hear all day.

Every set has the same seven cues (start, stop, done, ramble, cancel, error, learn) played on its
own "instrument". The rules that keep them calm:
  * low notes only: fundamentals between about 110 and 330 Hz, nothing above an octave and a
    half over that, and every sound low-passed (no fizz, no sparkle);
  * gentle starts (a raised-cosine fade-in of several milliseconds, so no clicks) and short,
    natural decays;
  * consonant intervals (fourths, fifths, thirds), quiet levels, no beeping square waves.

Writes ui/sounds/sets/<set>/<cue>.wav (48 kHz, 16-bit, mono) and ui/sounds/sets/index.json, and
prints each file's length, peak and spectral centre so the "low and soft" rule can be checked.
Run: python scripts/make_sounds.py
"""
import json
import math
import os
import wave

import numpy as np

try:
    from scipy.signal import lfilter
except ImportError:  # the plain loop below does the same, only slower
    lfilter = None

SR = 48_000
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "ui", "sounds", "sets")
RNG = np.random.default_rng(7)

# ---------------------------------------------------------------------------------------------
# Building blocks
# ---------------------------------------------------------------------------------------------


def biquad(x, kind, fc, q=0.707):
    """RBJ biquad (lowpass, highpass or bandpass), run forwards once."""
    w0 = 2 * math.pi * fc / SR
    alpha = math.sin(w0) / (2 * q)
    cos = math.cos(w0)
    if kind == "lowpass":
        b = [(1 - cos) / 2, 1 - cos, (1 - cos) / 2]
    elif kind == "highpass":
        b = [(1 + cos) / 2, -(1 + cos), (1 + cos) / 2]
    else:  # bandpass, constant 0 dB peak
        b = [alpha, 0.0, -alpha]
    a = [1 + alpha, -2 * cos, 1 - alpha]
    b = [v / a[0] for v in b]
    a1, a2 = a[1] / a[0], a[2] / a[0]
    if lfilter is not None:
        return lfilter(b, [1.0, a1, a2], x)
    y = np.zeros_like(x)
    x1 = x2 = y1 = y2 = 0.0
    for i, xi in enumerate(x):
        yi = b[0] * xi + b[1] * x1 + b[2] * x2 - a1 * y1 - a2 * y2
        x2, x1, y2, y1 = x1, xi, y1, yi
        y[i] = yi
    return y


def fade_in(n, ms):
    k = max(1, int(SR * ms / 1000))
    e = np.ones(n)
    k = min(k, n)
    e[:k] = 0.5 - 0.5 * np.cos(np.linspace(0, math.pi, k))
    return e


def pink_noise(n):
    """Voss-style pink-ish noise: white noise through a gentle 1/f tilt."""
    white = RNG.standard_normal(n)
    # Three one-pole filters summed approximate a -3 dB/octave slope well enough here.
    out = np.zeros(n)
    for coef, gain in ((0.99886, 0.0555179), (0.99332, 0.0750759), (0.96900, 0.1538520)):
        state = 0.0
        for i in range(n):
            state = coef * state + white[i] * gain
            out[i] += state
    return out / (np.max(np.abs(out)) + 1e-9)


def tonal(freq, dur, attack_ms, decay_s, partials, glide=0.0, glide_ms=45, vibrato=0.0):
    """A pitched note. partials: (ratio, amplitude, decay multiplier). glide: fractional pitch
    change that settles over glide_ms (positive rises)."""
    n = int(SR * dur)
    t = np.arange(n) / SR
    settle = 1 - np.exp(-t / (glide_ms / 1000))
    # Starts at freq * (1 - glide) and settles on freq: a positive glide rises into the note.
    f = freq * (1 - glide * (1 - settle))
    if vibrato:
        f = f * (1 + vibrato * np.sin(2 * math.pi * 5.0 * t))
    phase = 2 * math.pi * np.cumsum(f) / SR
    out = np.zeros(n)
    for ratio, amp, dmul in partials:
        out += amp * np.sin(phase * ratio) * np.exp(-t / (decay_s * dmul))
    return out * fade_in(n, attack_ms)


def noise_swell(fc, dur, attack_ms, decay_s, q=1.2, sweep=0.0):
    """Filtered breath: pink noise through a band-pass whose centre can drift (sweep)."""
    n = int(SR * dur)
    t = np.arange(n) / SR
    base = pink_noise(n)
    # Approximate a moving filter by cross-fading two fixed ones.
    a = biquad(base, "bandpass", fc * (1 - sweep / 2), q)
    b = biquad(base, "bandpass", fc * (1 + sweep / 2), q)
    mix = np.clip(t / dur, 0, 1)
    out = a * (1 - mix) + b * mix
    return out * fade_in(n, attack_ms) * np.exp(-t / decay_s)


def place(events, total_s):
    """Mix (start_s, samples) events into one buffer."""
    n = int(SR * total_s)
    out = np.zeros(n)
    for start, x in events:
        i = int(SR * start)
        j = min(n, i + len(x))
        out[i:j] += x[: j - i]
    return out


def finish(x, lowpass_hz, highpass_hz=70, peak_db=-9.0):
    x = biquad(x, "highpass", highpass_hz, 0.707)
    x = biquad(x, "lowpass", lowpass_hz, 0.6)
    x = biquad(x, "lowpass", lowpass_hz * 1.4, 0.7)  # second pass: a softer, steeper top
    # Trim the silent tail (below -60 dB of the peak), keep a 15 ms fade-out.
    peak = np.max(np.abs(x)) + 1e-12
    loud = np.where(np.abs(x) > peak * 10 ** (-60 / 20))[0]
    end = min(len(x), (loud[-1] if len(loud) else len(x)) + int(SR * 0.015))
    x = x[:end]
    k = min(len(x), int(SR * 0.015))
    x[-k:] *= np.linspace(1, 0, k) ** 2
    x -= np.mean(x)
    x *= 10 ** (peak_db / 20) / (np.max(np.abs(x)) + 1e-12)
    return x


# ---------------------------------------------------------------------------------------------
# Instruments: (freq, amplitude, glide) -> samples
# ---------------------------------------------------------------------------------------------


def i_hush(f, amp, glide):
    return amp * tonal(f, 0.42, 9, 0.075, [(1, 1.0, 1.0), (2, 0.06, 0.6)], glide)


def i_felt(f, amp, glide):
    body = tonal(f, 0.6, 6, 0.15, [(1, 1.0, 1.0), (2, 0.32, 0.6), (3, 0.12, 0.42), (4, 0.05, 0.3)], glide * 0.5)
    n = len(body)
    thump = biquad(RNG.standard_normal(n), "lowpass", 380, 0.7) * np.exp(-np.arange(n) / SR / 0.012) * fade_in(n, 2)
    return amp * (body + 0.25 * thump / (np.max(np.abs(thump)) + 1e-9))


def i_oak(f, amp, glide):
    return amp * tonal(f, 0.45, 5, 0.12, [(1, 1.0, 1.0), (3.93, 0.16, 0.32), (9.2, 0.03, 0.18)], glide * 0.3)


def i_kalimba(f, amp, glide):
    return amp * tonal(f, 0.75, 4, 0.24, [(1, 1.0, 1.0), (5.95, 0.07, 0.22), (2.0, 0.05, 0.5)], glide * 0.3)


def i_bubble(f, amp, glide):
    # A bubble's pitch rises as it closes: start well below and glide up into the note.
    return amp * tonal(f, 0.30, 6, 0.055, [(1, 1.0, 1.0), (2, 0.04, 0.5)], 0.32 + glide, glide_ms=28)


def i_breeze(f, amp, glide):
    return amp * noise_swell(f * 1.9, 0.5, 34, 0.09, q=1.1, sweep=glide * 6)


def i_bowl(f, amp, glide):
    return amp * tonal(f, 1.0, 8, 0.32, [(1, 1.0, 1.0), (2.71, 0.30, 0.7), (5.15, 0.08, 0.45)], glide * 0.2)


def i_dusk(f, amp, glide):
    return amp * tonal(f, 0.6, 26, 0.13, [(1, 1.0, 1.0), (1.5, 0.45, 0.9), (2, 0.10, 0.6)], glide * 0.5, vibrato=0.002)


def i_thock(f, amp, glide):
    body = tonal(f * 0.6, 0.18, 2, 0.032, [(1, 1.0, 1.0), (2, 0.18, 0.5)], -0.14 + glide * 0.3, glide_ms=18)
    n = len(body)
    click = biquad(RNG.standard_normal(n), "bandpass", 900, 1.4) * np.exp(-np.arange(n) / SR / 0.006) * fade_in(n, 1.2)
    return amp * (body + 0.18 * click / (np.max(np.abs(click)) + 1e-9))


def i_pebble(f, amp, glide):
    n = int(SR * 0.16)
    burst = RNG.standard_normal(n) * np.exp(-np.arange(n) / SR / 0.004) * fade_in(n, 1.0)
    ring = biquad(burst, "bandpass", f * 1.45 * (1 + glide), 7.0)
    ring += 0.5 * biquad(burst, "bandpass", f * 1.45 * 2.3 * (1 + glide), 9.0)
    return amp * ring / (np.max(np.abs(ring)) + 1e-9) * np.exp(-np.arange(n) / SR / 0.045)


def i_deep(f, amp, glide):
    return amp * tonal(f * 0.5, 0.45, 12, 0.09, [(1, 1.0, 1.0), (2, 0.42, 0.7), (3, 0.08, 0.5)], glide * 0.5)


def i_lull(f, amp, glide):
    return amp * tonal(f * 0.85, 0.7, 42, 0.16, [(1, 1.0, 1.0), (2, 0.05, 0.6)], glide * 0.5)


# ---------------------------------------------------------------------------------------------
# Sets and cue patterns
# ---------------------------------------------------------------------------------------------

# id, name, description, instrument, base note (Hz), low-pass (Hz), level (dB peak)
SETS = [
    ("hush", "Hush", "A pure, rounded hum. The quietest.", i_hush, 261.6, 1500, -10),
    ("felt", "Felt", "A muted piano note, felt on wood.", i_felt, 196.0, 2000, -9),
    ("oak", "Oak", "A soft wooden marimba.", i_oak, 220.0, 2400, -9),
    ("kalimba", "Kalimba", "A gentle thumb piano.", i_kalimba, 246.9, 2200, -10),
    ("bubble", "Bubble", "A low, round bloop.", i_bubble, 220.0, 1400, -9),
    ("breeze", "Breeze", "A breath of air. No notes at all.", i_breeze, 300.0, 1600, -15),  # retired, see RETIRED
    ("bowl", "Bowl", "A small singing bowl.", i_bowl, 196.0, 1900, -12),  # retired, see RETIRED
    ("dusk", "Dusk", "Two warm notes together, like a soft pad.", i_dusk, 196.0, 1500, -11),
    ("thock", "Thock", "A deep, quiet keyboard thock.", i_thock, 220.0, 2600, -9),
    ("pebble", "Pebble", "A small stone on wood. Very short.", i_pebble, 260.0, 2400, -10),
    ("deep", "Deep", "A low hum, best on good speakers or headphones.", i_deep, 220.0, 1100, -7),
    ("lull", "Lull", "A slow, soft swell. The calmest start.", i_lull, 233.1, 1400, -11),
]

# Sets taken out of the app (breeze, removed 15 Oct 2026). They are still synthesised so the shared
# random generator stays in step, but never written.
# They stay in SETS and are still synthesised, because every set draws from the one shared random
# generator (RNG) in this order: skipping one would change the sets after it. They are never
# written, and they are left out of index.json.
RETIRED = {"breeze", "bowl"}

# Each cue: list of (pitch ratio, start time s, amplitude, glide) and its total length.
CUES = {
    "start": ([(1.0, 0.0, 1.0, 0.04)], 0.55),
    "stop": ([(0.75, 0.0, 0.9, -0.04)], 0.55),
    "done": ([(1.0, 0.0, 0.8, 0.0), (1.5, 0.085, 0.72, 0.0)], 0.75),
    "ramble": ([(1.0, 0.0, 0.8, 0.0), (1.335, 0.1, 0.72, 0.0)], 0.75),
    "cancel": ([(1.0, 0.0, 0.75, 0.0), (0.75, 0.095, 0.62, -0.02)], 0.75),
    "error": ([(0.667, 0.0, 0.85, 0.0), (0.63, 0.13, 0.75, -0.02)], 0.8),
    "learn": ([(1.0, 0.0, 0.62, 0.0), (1.26, 0.075, 0.58, 0.0), (1.5, 0.15, 0.55, 0.0)], 0.9),
}


def write_wav(path, x):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    data = (np.clip(x, -1, 1) * 32767).astype("<i2").tobytes()
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SR)
        w.writeframes(data)


def centroid(x):
    spec = np.abs(np.fft.rfft(x * np.hanning(len(x)))) ** 2
    freqs = np.fft.rfftfreq(len(x), 1 / SR)
    return float(np.sum(freqs * spec) / (np.sum(spec) + 1e-12))


def main():
    index = []
    print(f"{'set':8} {'cue':7} {'ms':>5} {'peak dB':>8} {'centre Hz':>10}")
    worst = 0.0
    for sid, name, desc, inst, base, lp, level in SETS:
        for cue, (notes, total) in CUES.items():
            events = [(start, inst(base * ratio, amp, glide)) for ratio, start, amp, glide in notes]
            if sid in RETIRED:
                continue  # synthesised above only to keep the RNG in step; never written
            x = finish(place(events, total + 0.6), lp, 50 if sid == "deep" else 70, level)
            write_wav(os.path.join(OUT, sid, f"{cue}.wav"), x)
            c = centroid(x)
            worst = max(worst, c)
            print(f"{sid:8} {cue:7} {len(x) * 1000 // SR:5d} {20 * math.log10(np.max(np.abs(x))):8.1f} {c:10.0f}")
        if sid not in RETIRED:
            index.append({"id": sid, "name": name, "description": desc})
    with open(os.path.join(OUT, "index.json"), "w", encoding="utf-8") as f:
        json.dump({"default": "bubble", "sets": index}, f, indent=2)
    print(f"\n{len(index)} sets, highest spectral centre {worst:.0f} Hz")


if __name__ == "__main__":
    main()
