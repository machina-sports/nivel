# How Nivel works

## The processing chain (`nivel-dsp`)

Everything runs on 10 ms mono frames at 48 kHz (480 samples), the format RNNoise expects.

1. **High-pass filter** (2nd-order Butterworth, 80 Hz). Removes rumble, desk thumps and DC offset that waste headroom and confuse level detection.
2. **Noise suppression** with RNNoise via [nnnoiseless](https://github.com/jneem/nnnoiseless). Besides cleaning the signal, it returns a per-frame *voice probability*, which drives everything after it. `--strength` mixes the denoised signal with the original (delayed by one frame so they stay aligned).
3. **Voice detection**: a frame counts as speech when RNNoise is at least 50 % confident *and* the frame is above the tracked noise floor. With noise suppression off, an energy detector against the noise floor is used instead.
4. **Voice-gated AGC**. The speech level is averaged in the power domain over voiced frames only (about one second). The gain moves toward `target − speech level`, limited to `--max-gain`, rising at most 6 dB/s and falling at most 20 dB/s, faster during the first 1.5 s of speech. During pauses the gain holds, so the room is never pumped up.
5. **Soft gate**. When nobody has talked for 300 ms, the signal is turned down by `--gate` dB over ~150 ms. It reopens within a frame when speech returns. The gaps get quieter, but the line never sounds dead.
6. **Look-ahead limiter**. The signal is delayed 5 ms while a sliding-window minimum of the required gain lets the envelope duck *before* each peak arrives. Release is 80 ms, and a final clamp guarantees the `--ceiling`.

Each frame reports its measurements (`FrameStats`): input and output level and peak, voice probability, AGC gain, gate and limiter reduction, noise floor, and clipping / digital silence flags.

## The volume rider

`nivel ride` doesn't touch the audio at all. It runs the same analysis on the raw mic signal and moves the operating system's input volume:

- When the input clips (at least 2 frames above −1 dBFS), the volume drops by 10 % immediately.
- Otherwise, after at least 0.8 s of speech and 1.5 s since the previous move, if the speech level is more than 3 dB off target, it moves the slider by 0.008 per dB of error (8 % at most).
- It is closed-loop, so it doesn't need to know how each OS maps slider position to gain.
- If someone else moves the slider (you, or a meeting app's own auto-volume), it re-syncs and, if it keeps happening, tells you.

OS backends: CoreAudio `kAudioDevicePropertyVolumeScalar` and `kAudioDevicePropertyMute` on macOS, `IAudioEndpointVolume` on Windows, and `pactl` on Linux (works with PulseAudio and PipeWire).

## The real-time engine (`nivel-core`)

```text
mic callback ──ring──► DSP thread: resample → chain → resample (drift) ──ring──► output callback
```

- **Callbacks do almost nothing**: they downmix and push into, or pop from, lock-free ring buffers ([rtrb](https://github.com/mgeier/rtrb)). No allocation, no locks, no processing on the audio threads.
- **Downmix**: channels are averaged, except when one channel carries all the signal (an interface with the mic on input 1), which is then used alone so level and clip detection are not diluted. `--channel` forces one.
- **Resampling** uses [rubato](https://github.com/HEnquist/rubato) polynomial resamplers: device rate → 48 kHz before the chain, 48 kHz → output rate after it.
- **Clock drift**: the mic and the virtual device run on different clocks. The output resampler's ratio is nudged (at most ±0.2 %) to keep the output queue around 20 ms (or two device periods). The output waits for that cushion before starting and rebuilds it after an underrun, so a hiccup costs one short gap instead of a burst of crackles.
- **Latency**: roughly device period + 10 ms (RNNoise) + 5 ms (limiter) + ~20 ms queue + device period, about 50 ms in total.

## Staying alive: watchdog and supervisor

The `Watchdog` looks at telemetry snapshots (taken every 100 ms by the CLI) and raises:

| Condition | Action |
| --- | --- |
| No mic callback for 2 s (or output, when there is one) | Rebuild the engine |
| Fatal stream error (device unplugged, format changed) | Rebuild the engine |
| Pure digital silence for 3 s | Alert: mute switch, OS mute or missing permission |
| Pure digital silence for 6 s | Reopen the device once, then at most every 30 s |
| More than 1 % of frames clipping in 5 s | Alert |
| Speech pinned at maximum AGC boost for 8 s | Alert: too far from the mic |
| Any xrun, underrun or drop in 5 s | Alert |

The `Supervisor` owns the engine and:

- retries with exponential backoff (0.5 s to 5 s) while nothing works, recreating the host connection every few failures in case the sound server restarted;
- when the requested mic is missing, runs on the default mic meanwhile and switches back as soon as the requested one returns;
- with no mic requested, follows the system default mic when it changes;
- never captures from a virtual device by default, so it can't record its own output.

## Virtual microphones

| OS | How |
| --- | --- |
| Linux | `pactl load-module module-null-sink` (Nivel plays into it) plus `module-remap-source` on its monitor, exposed as "Nivel Microphone". Both are unloaded on exit. Works on PulseAudio and PipeWire (pipewire-pulse). |
| macOS | Plays into BlackHole (or Rogue Amoeba Loopback). A bundled AudioServerPlugIn driver is on the roadmap. |
| Windows | Plays into VB-CABLE ("CABLE Input"); apps record from "CABLE Output". A bundled driver is on the roadmap. |
