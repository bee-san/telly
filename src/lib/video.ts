/** Plays the audio track `listed` of those a video lists: a rip that plays as it is has all its tracks, and starts on its first. */
export function pickAudio(el: HTMLVideoElement, listed: number) {
  const tracks = (el as HTMLVideoElement & { audioTracks?: ArrayLike<{ enabled: boolean }> }).audioTracks;
  if (tracks?.[listed] && !tracks[listed].enabled) tracks[listed].enabled = true;
}

interface Boost {
  source: MediaElementAudioSourceNode;
  pre: GainNode;
  squash: DynamicsCompressorNode;
  post: GainNode;
  on: boolean;
}

let sound: AudioContext | null = null;
const boosts = new WeakMap<HTMLMediaElement, Boost>();

/** Turns Boost dialogue on or off for a video playing at `volume`: quiet speech comes up and loud music and effects come down. */
export function boostDialogue(el: HTMLMediaElement, on: boolean, volume: number) {
  let b = boosts.get(el);
  if (!b) {
    // A video that never had it on keeps playing straight to the speakers.
    if (!on) return;
    sound ??= new AudioContext();
    const squash = new DynamicsCompressorNode(sound, { threshold: -30, knee: 12, ratio: 4, attack: 0.003, release: 0.25 });
    b = { source: sound.createMediaElementSource(el), pre: sound.createGain(), squash, post: sound.createGain(), on: false };
    b.pre.connect(b.squash).connect(b.post).connect(sound.destination);
    b.source.connect(sound.destination);
    boosts.set(el, b);
  }
  if (!sound) return;
  if (b.on !== on) {
    b.source.disconnect();
    b.source.connect(on ? b.pre : sound.destination);
    b.on = on;
  }
  // The video's own volume is already in what comes in, so it's taken out before the compressor
  // and put back after it: turned down, quiet speech is still brought up as much.
  b.pre.gain.value = volume > 0 ? 1 / volume : 1;
  b.post.gain.value = volume;
  if (sound.state === "suspended") sound.resume().catch(() => {});
}
